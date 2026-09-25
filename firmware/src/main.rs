//! nitride-nano — USB-PD bench power supply firmware (STM32G474, Embassy).
//!
//! Structure: this entry point owns all peripherals and runs the cooperative
//! main loop (input → ADC → sweep/supply tick → PD poll → EEPROM step). A single
//! Embassy task (`ui_task`) renders the OLED. Shared state flows through the
//! `APP_STATE` mutex and the two I2C bus mutexes in [`nitride_firmware::runtime`];
//! the lock order (APP_STATE before either bus, never both buses) documented
//! there is load-bearing.
//!
//! Timing contract (do not change without re-tuning the control loop):
//! input 5 ms, ADC 2 ms, supply tick 1 ms, PD/INA228 100 ms, EEPROM step every
//! pass, plus a 100 µs yield at the bottom of the loop.
//!
//! Service mode ([`nitride_firmware::service`]) lets a running board hand off to
//! the ST ROM bootloader so it can be reflashed over UART; see
//! [`nitride_firmware::hal::bootloader`].

#![no_std]
#![no_main]

use core::sync::atomic::Ordering;

use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_stm32::adc::{Adc, Resolution, SampleTime};
use embassy_stm32::dac::{DacCh1, DacChannel, Value};
use embassy_stm32::exti::ExtiInput;
use embassy_stm32::gpio::{Input, Level, Output, Pull, Speed};
use embassy_stm32::i2c::{Config as I2cConfig, I2c};
use embassy_stm32::rcc::*;
use embassy_stm32::timer::qei::{Qei, QeiPin};
use embassy_stm32::usart::{Config as UartConfig, Uart};
use embassy_stm32::{bind_interrupts, peripherals, Config};
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Instant, Timer};
use panic_probe as _;

use nitride_firmware::board;
use nitride_firmware::control::supply::SupplyController;
use nitride_firmware::control::sweep::SweepController;
use nitride_firmware::drivers::tps26750::Tps26750;
use nitride_firmware::eeprom_workflow::EepromWorkflow;
use nitride_firmware::hal::bootloader;
use nitride_firmware::hal::converter_enable::ConverterEnable;
use nitride_firmware::pd::manager::PdManager;
use nitride_firmware::runtime::{APP_STATE, I2C_PD_BUS, I2C_UI_BUS};
use nitride_firmware::sense::adc_sense::{AdcSense, TelemetryFilter};
use nitride_firmware::sense::ina_sense::InaSense;
use nitride_firmware::service::{self, service_uart_task};
use nitride_firmware::state::{AppState, EepromUiSnapshot, MenuScreen};
use nitride_firmware::ui::input::InputHandler;
use nitride_firmware::ui::menu::apply_input;
use nitride_firmware::ui::task::ui_task;

bind_interrupts!(struct Irqs {
    I2C1_EV => embassy_stm32::i2c::EventInterruptHandler<peripherals::I2C1>;
    I2C1_ER => embassy_stm32::i2c::ErrorInterruptHandler<peripherals::I2C1>;
    I2C3_EV => embassy_stm32::i2c::EventInterruptHandler<peripherals::I2C3>;
    I2C3_ER => embassy_stm32::i2c::ErrorInterruptHandler<peripherals::I2C3>;
    USART3 => embassy_stm32::usart::InterruptHandler<peripherals::USART3>;
});

/// Copy the workflow's display-facing fields into the shared state so the UI
/// task can render them without ever touching the workflow itself.
fn sync_eeprom_ui(app: &mut AppState, workflow: &EepromWorkflow) {
    app.eeprom_ui = EepromUiSnapshot {
        title: workflow.title(),
        message: workflow.message(),
        progress_percent: workflow.progress_percent(),
    };
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // Service mode, handoff leg: a previous boot recorded a request in `.uninit`
    // RAM and reset. Take it here — before the executor starts or any peripheral
    // is touched — so the ROM bootloader is entered from a pristine machine
    // state. Diverges.
    if bootloader::take_request() {
        defmt::info!("service mode: entering ROM bootloader");
        bootloader::jump_to_system_bootloader();
    }

    let mut config = Config::default();
    config.rcc.mux.adc12sel = mux::Adcsel::SYS;
    config.rcc.mux.adc345sel = mux::Adcsel::SYS;

    let p = embassy_stm32::init(config);
    defmt::info!("boot: clocks up");

    // VREF+ is hard-tied to +3V3 on this board, so the ADC/DAC reference is the
    // 3.3 V rail and the internal VREFBUF must NOT drive the pin. Leave VREFBUF
    // untouched in its reset state (external-reference mode: ENVR=0, HIZ=1) —
    // that is the correct configuration for this board and, unlike
    // `VoltageReferenceBuffer::new(..)` with `Hiz::HIGH_Z`/`CONNECTED`, it has
    // no `while VRR { }` wait loop in it. See `board::DAC_VREF_MV`.
    // NOTE: do not re-enable VREFBUF here — if VRR reads high at that point the
    // embassy init loop never exits and the firmware hangs before the OLED is
    // ever initialised.

    let mut dac_cv: DacCh1<'_, embassy_stm32::peripherals::DAC1, embassy_stm32::mode::Blocking> =
        DacChannel::new_blocking(p.DAC1, p.PA4);
    let mut dac_cc: DacCh1<'_, embassy_stm32::peripherals::DAC2, embassy_stm32::mode::Blocking> =
        DacChannel::new_blocking(p.DAC2, p.PA6);
    // The CV DAC is inverted: full-scale code = minimum output. Park it there
    // before anything else can enable the converter.
    dac_cv.set(Value::Bit12Right(board::DAC_MAX_CODE));
    dac_cc.set(Value::Bit12Right(0));
    defmt::info!("boot: dac parked");

    let mut adc1 = Adc::new(p.ADC1);
    adc1.set_resolution(Resolution::BITS12);
    adc1.set_sample_time(SampleTime::CYCLES640_5);
    let mut adc2 = Adc::new(p.ADC2);
    adc2.set_resolution(Resolution::BITS12);
    adc2.set_sample_time(SampleTime::CYCLES47_5);
    let mut adc5 = Adc::new(p.ADC5);
    adc5.set_resolution(Resolution::BITS12);
    adc5.set_sample_time(SampleTime::CYCLES47_5);

    let mut pin_vout = p.PA0;
    let mut pin_temp_conv = p.PA1;
    let mut pin_isense = p.PA3;
    let mut pin_vbus = p.PA7;
    let mut pin_temp_in = p.PA9;

    defmt::timestamp!("{=u64:us}", { embassy_time::Instant::now().as_micros() });

    let mut conv_en = ConverterEnable::new(Output::new(
        p.PA11,
        if board::CONVERTER_DISABLE_ACTIVE_HIGH {
            Level::High
        } else {
            Level::Low
        },
        Speed::Low,
    ));
    conv_en.set_enabled(false);

    let btn1 = Input::new(p.PB9, Pull::Up);
    let btn2 = Input::new(p.PB10, Pull::Up);
    let btn3 = Input::new(p.PB11, Pull::Up);
    let enc_btn = Input::new(p.PB4, Pull::Up);

    // Service mode, trigger #2: BTN1 held through power-up. Deterministic and
    // needs no UART, so it doubles as the bring-up test for the handoff path.
    // The output is already parked (DACs and converter disable above), and the
    // subsequent boot consumes the request before this check, so it cannot loop.
    //
    // Confirm the level twice: a single early sample can catch a still-settling
    // pin or contact bounce, and this path resets the MCU.
    if board::SERVICE_BOOT_HOLD {
        Timer::after(Duration::from_millis(50)).await;
        if btn1.is_low() {
            Timer::after(Duration::from_millis(200)).await;
            if btn1.is_low() {
                defmt::info!("service mode: BTN1 held at boot");
                bootloader::request_on_next_boot();
            }
        }
    }

    let qei = Qei::new(p.TIM4, QeiPin::new(p.PB6), QeiPin::new(p.PA12));
    let mut enc_last: u16 = qei.count();
    // Carries the leftover quadrature counts between polls so a detent split
    // across two 5 ms windows still produces exactly one `EncTurn` event.
    let mut enc_accum: i32 = 0;
    let mut pd_irq = ExtiInput::new(p.PB13, p.EXTI13, Pull::Up);

    // I2C configs are per-bus because the two buses need opposite timeout
    // behaviour.
    //
    // embassy's I2C waits in a *blocking* spin loop (`wait_af`/`wait_rxne`/...
    // call `timeout.check()`), AND its async DMA path wraps the transfer in that
    // same deadline. So a long timeout on a dead bus freezes the executor, while
    // a short timeout on the UI bus aborts OLED transactions that were simply
    // unlucky enough to be in flight while the main loop was blocked polling the
    // PD bus — which truncates `flush_partial` and leaves whole GDDRAM pages
    // unwritten (random speckle).
    //
    // UI bus (I2C3, OLED/EEPROM): generous. A healthy transfer is <1 ms; this
    // only has to outlast the main loop's worst-case PD poll.
    let mut i2c_ui_config = I2cConfig::default();
    i2c_ui_config.frequency = embassy_stm32::time::Hertz::khz(400);
    i2c_ui_config.timeout = Duration::from_millis(250);

    // PD bus (I2C1, TPS26750/INA228): short, so a missing or stuck PD controller
    // fails fast instead of freezing the executor (and the OLED) for a second.
    let mut i2c_pd_config = I2cConfig::default();
    i2c_pd_config.frequency = embassy_stm32::time::Hertz::khz(400);
    i2c_pd_config.timeout = Duration::from_millis(20);

    // UI bus (I2C3): SSD1306 OLED, plus the CAT24C512 config EEPROM when
    // JP8/JP9 are bridged.
    let i2c_ui = I2c::new(
        p.I2C3, p.PA8, p.PB5, Irqs, p.DMA1_CH3, p.DMA1_CH4, i2c_ui_config,
    );
    let ui_bus = I2C_UI_BUS.init(Mutex::new(i2c_ui));

    // PD bus (I2C1): TPS26750 USB-PD controller and INA228 input monitor.
    // rev2 routes these to PC4/PB7, an I2C pin pair no single peripheral can
    // drive, so SCL is bodged to PB8 and SDA stays on PB7.
    let i2c_pd = I2c::new(
        p.I2C1, p.PB8, p.PB7, Irqs, p.DMA1_CH1, p.DMA1_CH2, i2c_pd_config,
    );
    let pd_bus = I2C_PD_BUS.init(Mutex::new(i2c_pd));
    defmt::info!("boot: i2c up");

    // Service mode, trigger #1: the on-board FT234XD sits on USART3
    // (TX=PC10, RX=PC11) — the same port the ROM bootloader listens on. We only
    // receive here, watching for the AN3155 sync byte so that merely opening
    // the programmer hands the device over. DMA1_CH5/CH6 are the channels left
    // free by the two I2C buses.
    //
    // RX MUST be pulled up: with the FT234XD unpowered (USB detached) its TXD is
    // high-Z, and a floating RX generates framing/noise bytes that can include
    // the 0x7F sync value — which would park the output and reset into the ROM
    // bootloader on an otherwise normal power-up.
    let service_uart = if board::SERVICE_UART_AUTODETECT {
        let mut uart_config = UartConfig::default();
        uart_config.baudrate = board::SERVICE_UART_BAUD;
        uart_config.rx_pull = Pull::Up;
        match Uart::new(
            p.USART3,
            p.PC11,
            p.PC10,
            Irqs,
            p.DMA1_CH5,
            p.DMA1_CH6,
            uart_config,
        ) {
            Ok(uart) => Some(uart),
            Err(_) => {
                defmt::error!("service UART init failed; UART handoff disabled");
                None
            }
        }
    } else {
        None
    };
    defmt::info!(
        "boot: service uart {}",
        if service_uart.is_some() { "up" } else { "off" }
    );

    let app = AppState::default();
    let app_state = APP_STATE.init(Mutex::new(app));

    let mut supply = SupplyController::new();
    let mut sweep = SweepController::new();
    let mut sense = AdcSense::new();
    let mut input = InputHandler::new();
    let mut tele_filter = TelemetryFilter::new();
    let mut pd_mgr = PdManager::new();
    let mut tps = Tps26750::new(board::TPS26750_ADDR);
    let mut ina = InaSense::new();
    let mut eeprom_workflow = EepromWorkflow::new();

    {
        let mut i2c = pd_bus.lock().await;
        let _ = ina.init(&mut i2c).await;
        // The TPS26750 loads its application firmware from EEPROM and is not
        // guaranteed to answer this early, so it is not probed here: PdManager
        // owns a presence watchdog that re-probes it until it responds.
    }
    defmt::info!("boot: ina init done");

    // The ISMON zero is a bench-calibrated constant (`board::ISENSE_ZERO_MV`),
    // deliberately NOT learned here: the LT8390A powers its ISMON buffer down
    // with the rest of the chip while EN/UVLO is low, so a sample taken with the
    // converter parked does not see the operating offset. Log what is in use.
    defmt::info!(
        "boot: isense zero = {} counts ({} mV), gain {} mV/A",
        sense.zero_raw(),
        board::ISENSE_ZERO_MV,
        board::ISENSE_MV_PER_A
    );

    spawner.spawn(ui_task(app_state, ui_bus)).unwrap();
    if let Some(uart) = service_uart {
        spawner.spawn(service_uart_task(uart)).unwrap();
    }

    let mut t_adc = Instant::now();
    let mut t_supply = Instant::now();
    let mut t_input = Instant::now();
    let mut t_pd = Instant::now();
    // Bring-up ISMON diagnostic cadence (see the `isense:` log below).
    let mut t_isense_log = Instant::now();
    // PD poll period. Backed off to a slow retry while the bus is not answering.
    let mut pd_period = Duration::from_millis(board::INA228_POLL_MS);

    loop {
        // Service mode, handoff leg: park the output, let it settle, then reset
        // with a request recorded so the next boot enters the ROM bootloader.
        // Reusing `supply.tick` keeps the inverted-CV and CC-zero behaviour in
        // one place; the APP_STATE lock is dropped before the settle delay.
        if service::SERVICE_REQUEST.swap(false, Ordering::SeqCst) {
            {
                let mut app = app_state.lock().await;
                app.supply.enabled = false;
                supply.tick(&mut app, &mut dac_cv, &mut dac_cc, &mut conv_en);
            }
            conv_en.set_enabled(false);
            defmt::info!("service mode: parking output before handoff");
            Timer::after(Duration::from_millis(board::SERVICE_PARK_SETTLE_MS)).await;
            bootloader::request_on_next_boot();
        }

        let now = Instant::now();

        if now.duration_since(t_input) >= Duration::from_millis(board::INPUT_POLL_MS) {
            t_input = now;
            let c = qei.count();
            enc_accum += (c.wrapping_sub(enc_last) as i16) as i32;
            enc_last = c;
            // Convert raw quadrature counts to detents. The remainder is kept so
            // a detent straddling two polls is not lost or double-counted.
            let detents = (enc_accum / board::ENCODER_COUNTS_PER_DETENT) as i16;
            enc_accum -= detents as i32 * board::ENCODER_COUNTS_PER_DETENT;
            input.poll(&btn1, &btn2, &btn3, &enc_btn, detents);
            if let Some(ev) = input.last_event {
                let mut app = app_state.lock().await;
                let previous_screen = app.ui.screen;
                if app.ui.screen == MenuScreen::EepromFlash {
                    eeprom_workflow.handle_input(ev);
                }
                apply_input(&mut app, ev);
                if previous_screen != app.ui.screen && app.ui.screen == MenuScreen::EepromFlash {
                    defmt::info!("Entered EEPROM flash screen");
                    sync_eeprom_ui(&mut app, &eeprom_workflow);
                }
            }
            input.clear_event();
        }

        if now.duration_since(t_adc) >= Duration::from_millis(board::ADC_SAMPLE_MS) {
            t_adc = now;
            let raw = sense.sample(
                &mut adc1,
                &mut adc2,
                &mut adc5,
                &mut pin_vout,
                &mut pin_isense,
                &mut pin_vbus,
                &mut pin_temp_conv,
                &mut pin_temp_in,
            );
            let mut app = app_state.lock().await;
            // The ADC filter only owns the output-side channels; carry the
            // INA228's input-side values across its whole-struct replacement.
            let prev_vin = app.telemetry.vin_mv;
            let prev_iin = app.telemetry.iin_ma;
            let prev_pin = app.telemetry.pin_mw;
            let prev_ina_temp = app.telemetry.ina_temp_c;
            let prev_ina_ok = app.telemetry.ina_ok;
            let mut filtered = tele_filter.filter(raw);
            filtered.iin_ma = prev_iin;
            filtered.pin_mw = prev_pin;
            filtered.ina_temp_c = prev_ina_temp;
            filtered.ina_ok = prev_ina_ok;
            if prev_ina_ok {
                // Prefer the INA228's input-bus voltage over the ADC divider.
                filtered.vin_mv = prev_vin;
            }
            app.telemetry = filtered;

            // Bring-up ISMON diagnostic. The raw PA3 count and the node voltage
            // are independent of the scaling constants, so comparing them at 0 A
            // vs a known load shows at a glance whether the LT8390A's monitor is
            // moving and what zero should be (calibrate with the output ON and
            // no load). LT8390A + R18 (2 mΩ) should give ~25 counts/A.
            if now.duration_since(t_isense_log) >= Duration::from_secs(1) {
                t_isense_log = now;
                let raw = sense.last_i_raw();
                let zero = sense.zero_raw();
                defmt::info!(
                    "isense: raw={} ({} mV) zero={} ({} mV) span={} mV vout={} mV iout={} mA vin={} mV iin={} mA",
                    raw,
                    raw * board::ADC_VREF_MV / 4096,
                    zero,
                    zero * board::ADC_VREF_MV / 4096,
                    raw.saturating_sub(zero) * board::ADC_VREF_MV / 4096,
                    app.telemetry.vout_mv,
                    app.telemetry.iout_ma,
                    app.telemetry.vin_mv,
                    app.telemetry.iin_ma
                );
            }
        }

        if now.duration_since(t_supply) >= Duration::from_millis(board::SUPPLY_TICK_MS) {
            t_supply = now;
            let mut app = app_state.lock().await;
            // Sweep first: it may move the setpoint/phase this very tick, and the
            // supply tick below pushes the resulting setpoint to the CV DAC.
            sweep.tick(&mut app);
            supply.tick(&mut app, &mut dac_cv, &mut dac_cc, &mut conv_en);
        }

        if now.duration_since(t_pd) >= pd_period {
            t_pd = now;
            let pd_started = Instant::now();
            let mut app = app_state.lock().await;
            let mut i2c = pd_bus.lock().await;
            pd_mgr.poll(&mut tps, &mut i2c, &mut app, &mut pd_irq).await;
            if pd_mgr.has_pending_request() {
                // Park the output (DACs + converter enable) before the input
                // rail is renegotiated.
                supply.tick(&mut app, &mut dac_cv, &mut dac_cc, &mut conv_en);
            }
            pd_mgr.negotiate(&mut tps, &mut i2c).await;
            ina.poll(&mut i2c, &mut app).await;

            // embassy's I2C driver waits for events in a blocking spin loop, so a
            // bus that does not answer blocks the whole executor (including
            // `ui_task`) for the per-transaction timeout. A healthy poll takes a
            // few ms; a timing-out one costs ~2 x the I2C timeout. Back off hard
            // when that happens so a missing PD controller cannot starve the UI.
            let pd_took = Instant::now().duration_since(pd_started);
            pd_period = if pd_took > Duration::from_millis(30) {
                Duration::from_secs(2)
            } else {
                Duration::from_millis(board::INA228_POLL_MS)
            };
        }

        {
            let screen = app_state.lock().await.ui.screen;
            if screen == MenuScreen::EepromFlash {
                let mut i2c = ui_bus.lock().await;
                eeprom_workflow.update(&mut i2c).await;
                let mut app = app_state.lock().await;
                sync_eeprom_ui(&mut app, &eeprom_workflow);
            }
        }

        Timer::after(Duration::from_micros(100)).await;
    }
}
