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
//! Reflashing on rev3 is over USB-C, using the STM32G474's built-in ROM USB DFU
//! bootloader on PA11/PA12. It is entered by a **hardware strap** — hold BOOT0
//! (`SW2`) across a reset (`SW3`) — so the application carries no bootloader
//! handoff code at all: the ROM loader only stays when BOOT0 is high at the
//! reset sampling instant, which a software jump cannot arrange.
//!
//! See `PCB/ECO-rev3-usb-dfu.md` for the ECO this pin map implements.

#![no_std]
#![no_main]

use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_stm32::adc::{Adc, Resolution, SampleTime};
use embassy_stm32::dac::{DacCh1, DacChannel, Value};
use embassy_stm32::exti::ExtiInput;
use embassy_stm32::gpio::{Input, Level, Output, OutputType, Pull, Speed};
use embassy_stm32::i2c::{Config as I2cConfig, I2c};
use embassy_stm32::rcc::*;
use embassy_stm32::timer::low_level::CountingMode;
use embassy_stm32::timer::qei::{Qei, QeiPin};
use embassy_stm32::timer::simple_pwm::{PwmPin, SimplePwm};
use embassy_stm32::{bind_interrupts, peripherals, Config};
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Instant, Timer};
use panic_probe as _;

use nitride_firmware::board;
use nitride_firmware::control::dac_cc::CcDac;
use nitride_firmware::control::dac_cv::CvDac;
use nitride_firmware::control::supply::SupplyController;
use nitride_firmware::control::sweep::SweepController;
use nitride_firmware::drivers::tps26750::Tps26750;
use nitride_firmware::eeprom_workflow::{EepromWorkflow, WorkflowState};
use nitride_firmware::hal::converter_enable::ConverterEnable;
use nitride_firmware::hal::high_gain::HighGain;
use nitride_firmware::hal::load_enable::LoadEnable;
use nitride_firmware::hal::output_switch::OutputSwitch;
use nitride_firmware::pd::manager::PdManager;
use nitride_firmware::runtime::{APP_STATE, I2C_PD_BUS, I2C_UI_BUS};
use nitride_firmware::sense::adc_sense::{AdcSense, TelemetryFilter};
use nitride_firmware::sense::ina_sense::InaSense;
use nitride_firmware::state::{AppState, EepromUiSnapshot, MenuScreen, OutInaTelemetry};
use nitride_firmware::ui::input::InputHandler;
use nitride_firmware::ui::led::led_task;
use nitride_firmware::ui::menu::apply_input;
use nitride_firmware::ui::task::ui_task;

bind_interrupts!(struct Irqs {
    // rev3 PD bus (TPS26750 + both INA228s).
    I2C2_EV => embassy_stm32::i2c::EventInterruptHandler<peripherals::I2C2>;
    I2C2_ER => embassy_stm32::i2c::ErrorInterruptHandler<peripherals::I2C2>;
    // rev3 UI bus (SSD1306 OLED + config EEPROM).
    I2C4_EV => embassy_stm32::i2c::EventInterruptHandler<peripherals::I2C4>;
    I2C4_ER => embassy_stm32::i2c::ErrorInterruptHandler<peripherals::I2C4>;
});

/// Copy the workflow's display-facing fields into the shared state so the UI
/// task can render them without ever touching the workflow itself.
fn sync_eeprom_ui(app: &mut AppState, workflow: &EepromWorkflow) {
    app.eeprom_ui = EepromUiSnapshot {
        title: workflow.title(),
        message: workflow.message(),
        progress_percent: workflow.progress_percent(),
        busy: workflow.state == WorkflowState::Flashing,
        failed: workflow.state == WorkflowState::Error,
    };
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
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

    // rev3 power path. Park all three in their safe state before anything else
    // can enable the converter: chip disabled, `CNV-EN-LOADEN` low (bottom gates
    // on, output grounded) and the load switch open. `PWR_G` has a 100 kΩ
    // pulldown on the board, so an MCU held in reset already leaves the load
    // disconnected.
    let mut conv_en = ConverterEnable::new(Output::new(
        p.PB1,
        if board::CONVERTER_DISABLE_ACTIVE_HIGH {
            Level::High
        } else {
            Level::Low
        },
        Speed::Low,
    ));
    conv_en.set_enabled(false);
    let mut load_en = LoadEnable::new(Output::new(p.PB3, Level::Low, Speed::Low));
    load_en.set_enabled(false);
    let mut pwr_g = OutputSwitch::new(Output::new(p.PB2, Level::Low, Speed::Low));
    pwr_g.set_closed(false);
    // CC sense range. The pin starts at ×1 (the widest window, and what the
    // previous revision always ran) and then follows the setpoint; `SupplyController`
    // only moves it while the stage is parked. The net has no external pull, so
    // driving it from boot matters.
    let mut high_gain = HighGain::new(
        Output::new(p.PB6, Level::Low, Speed::Low),
        board::CcRange::LowGain,
    );

    let btn1 = Input::new(p.PB9, Pull::Up);
    let btn2 = Input::new(p.PB10, Pull::Up);
    let btn3 = Input::new(p.PB11, Pull::Up);
    // rev3: the encoder button moved to PB0 to free PB4 for TIM3_CH1.
    let enc_btn = Input::new(p.PB0, Pull::Up);

    // rev3: the quadrature encoder is TIM3 on PB4/PB5. The Qei driver, the X4
    // decoding (`Sms::ENCODER_MODE_3`) and `board::ENCODER_COUNTS_PER_DETENT` are
    // unchanged — only the timer instance and pins move.
    let qei = Qei::new(p.TIM3, QeiPin::new(p.PB4), QeiPin::new(p.PB5));
    let mut enc_last: u16 = qei.count();
    // Carries the leftover quadrature counts between polls so a detent split
    // across two 5 ms windows still produces exactly one `EncTurn` event.
    let mut enc_accum: i32 = 0;
    let mut pd_irq = ExtiInput::new(p.PB13, p.EXTI13, Pull::Up);

    // Status LED: D31 on PA15 (TIM2_CH1). Driven as hardware PWM so the pattern
    // engine in `ui::led` can dim the healthy heartbeat precisely and keep it
    // steady even while a blocking I2C timeout stalls the executor.
    let led_pwm = SimplePwm::new(
        p.TIM2,
        Some(PwmPin::new(p.PA15, OutputType::PushPull)),
        None,
        None,
        None,
        embassy_stm32::time::Hertz::hz(board::LED_PWM_HZ),
        CountingMode::EdgeAlignedUp,
    );
    let led_ch = led_pwm.split().ch1;

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
    // UI bus (I2C4, OLED/EEPROM): generous. A healthy transfer is <1 ms; this
    // only has to outlast the main loop's worst-case PD poll.
    let mut i2c_ui_config = I2cConfig::default();
    i2c_ui_config.frequency = embassy_stm32::time::Hertz::khz(400);
    i2c_ui_config.timeout = Duration::from_millis(250);

    // PD bus (I2C2, TPS26750 + both INA228s): short, so a missing or stuck PD
    // controller fails fast instead of freezing the executor (and the OLED) for
    // a second.
    let mut i2c_pd_config = I2cConfig::default();
    i2c_pd_config.frequency = embassy_stm32::time::Hertz::khz(400);
    i2c_pd_config.timeout = Duration::from_millis(20);

    // UI bus (I2C4): SSD1306 OLED, plus the CAT24C512 config EEPROM when
    // JP8/JP9 are bridged. rev3 moved this from I2C3 (PA8/PB5) to free PA8 for
    // the PD bus and PB6 for `High-Gain`.
    let i2c_ui = I2c::new(
        p.I2C4, p.PC6, p.PB7, Irqs, p.DMA1_CH3, p.DMA1_CH4, i2c_ui_config,
    );
    let ui_bus = I2C_UI_BUS.init(Mutex::new(i2c_ui));

    // PD bus (I2C2): TPS26750 USB-PD controller, INA228 input monitor (U9) and
    // INA228 output monitor (U12). rev3 moved this from I2C1 (PB8/PB7) to free
    // PB8 for the BOOT0 strap; PC4/PA8 are a valid I2C2 pin pair, so the rev2
    // SCL bodge is gone.
    let i2c_pd = I2c::new(
        p.I2C2, p.PC4, p.PA8, Irqs, p.DMA1_CH1, p.DMA1_CH2, i2c_pd_config,
    );
    let pd_bus = I2C_PD_BUS.init(Mutex::new(i2c_pd));
    defmt::info!("boot: i2c up");

    let app = AppState::default();
    let app_state = APP_STATE.init(Mutex::new(app));

    let mut supply = SupplyController::new();
    let mut sweep = SweepController::new();
    let mut sense = AdcSense::new();
    let mut input = InputHandler::new();
    let mut tele_filter = TelemetryFilter::new();
    let mut pd_mgr = PdManager::new();
    let mut tps = Tps26750::new(board::TPS26750_ADDR);
    let mut ina_in = InaSense::new(board::INA228_IN_ADDR, board::INA228_IN_SHUNT_MOHM);
    let mut ina_out = InaSense::new(board::INA228_OUT_ADDR, board::INA228_OUT_SHUNT_MOHM);
    let mut eeprom_workflow = EepromWorkflow::new();

    {
        let mut i2c = pd_bus.lock().await;
        let _ = ina_in.init(&mut i2c).await;
        let _ = ina_out.init(&mut i2c).await;
        // The TPS26750 loads its application firmware from EEPROM and is not
        // guaranteed to answer this early, so it is not probed here: PdManager
        // owns a presence watchdog that re-probes it until it responds.
    }
    defmt::info!("boot: ina init done");

    // The ISMON zero is a bench-calibrated constant (`board::ISENSE_ZERO_MV`),
    // deliberately NOT learned here: the LT8390A powers its ISMON buffer down
    // with the rest of the chip while EN/UVLO is low, so a sample taken with the
    // converter parked does not see the operating offset. The zero is
    // range-independent — the 0.25 V offset is inside the chip, after the
    // differential — but the *gain* is not, so both are reported here. Which one
    // is live is printed by the `isense:` line with the active range.
    defmt::info!(
        "boot: isense zero = {} counts ({} mV); gain {} mV/A at x1, {} mV/A at x10",
        sense.zero_raw(),
        board::ISENSE_ZERO_MV,
        board::CcRange::LowGain.mv_per_a(),
        board::CcRange::HighGain.mv_per_a()
    );

    // Converter-enable pin and the two open-loop maps, logged once so a bench
    // measurement can be checked against the exact numbers the firmware used.
    // rev3 moved the `Q13` gate / `EN/UVLO` disable line from PA11 to PB1 and
    // added the `CNV-EN-LOADEN` (PB3) and `PWR_G` (PB2) stages.
    defmt::info!(
        "boot: conv-disable PB1, load-enable PB3 (CNV-EN-LOADEN), pwr_g PB2 (active_high={}), high-gain PB6, CV dac PA4, CC dac PA6",
        board::CONVERTER_DISABLE_ACTIVE_HIGH
    );
    defmt::info!(
        "boot: cv map: code0 -> {} mV, code{} -> {} mV (VREF {} mV, R19 {} R20 {} R36 {})",
        CvDac::vout_at_code_zero_mv(),
        board::DAC_MAX_CODE,
        CvDac::code_to_vout_mv(board::DAC_MAX_CODE),
        board::DAC_VREF_MV,
        board::CV_FB_TOP_OHM,
        board::CV_FB_BOTTOM_OHM,
        board::CV_SUM_OHM
    );
    defmt::info!(
        "boot: cc ctrl net R48={} R5={} R4={} VREF={} mV; code0 ctrl {} mV (latchoff {}, cold-start {})",
        board::CTRL_SERIES_OHM,
        board::CTRL_VREF_OHM,
        board::CTRL_GND_OHM,
        board::LT8390_VREF_MV,
        CcDac::ctrl_mv_for_code(0),
        board::LT8390_CTRL_LATCHOFF_MV,
        board::LT8390_CTRL_START_MV
    );
    // The CC window is the part's, not the firmware's: report the settable range
    // next to what the chip can *sustain* and what it needs to cold-start. rev3
    // has two ranges, selected by the `High-Gain` mux, so print both. The *loop
    // offsets* are 0 until they are re-measured on this board — the rev2 figure
    // (3650 mA) was taken on a different sense node.
    for r in [board::CcRange::LowGain, board::CcRange::HighGain] {
        defmt::info!(
            "boot: cc range {:?}: R_eff={} mOhm, ISMON {} mV/A, settable {}..{} mA, sustain {}..{} mA, cold-start {} mA, offset {} mA, set {} mA -> code {} (ctrl {} mV), set {} mA -> code {} (ctrl {} mV)",
            r,
            r.shunt_mohm(),
            r.mv_per_a(),
            r.set_min_ma(),
            r.set_max_ma(),
            r.min_ma(),
            r.max_ma(),
            r.start_min_ma(),
            r.offset_ma(),
            r.set_min_ma(),
            CcDac::setpoint_to_code(r.set_min_ma(), r),
            CcDac::ctrl_mv_for_code(CcDac::setpoint_to_code(r.set_min_ma(), r)),
            r.set_max_ma(),
            CcDac::setpoint_to_code(r.set_max_ma(), r),
            CcDac::ctrl_mv_for_code(CcDac::setpoint_to_code(r.set_max_ma(), r))
        );
    }
    defmt::info!(
        "boot: high-gain switches at iset <= {} mA; UI span {}..{} mA",
        board::HIGH_GAIN_MAX_SET_MA,
        board::CC_SET_MIN_MA,
        board::CC_SET_MAX_MA
    );
    defmt::info!(
        "boot: iout software trip margin {}%; supply snapshot every {} ms",
        board::IOUT_TRIP_MARGIN_PCT,
        board::SUPPLY_LOG_MS
    );
    // The rev3 power-up sequence, so the bench can check the scope against these
    // exact numbers. See `board::OUTPUT_*` and `control::sequencer`.
    defmt::info!(
        "boot: output seq: preloaden {} ms, ramp timeout {} ms (reached {}%), switch settle {} ms, require_load={} (min {} mA), require_ina={}, discharge {} ms, conv-off {} ms",
        board::OUTPUT_PRELOADEN_MS,
        board::OUTPUT_RAMP_TIMEOUT_MS,
        board::VOUT_REACHED_PCT,
        board::OUTPUT_SWITCH_SETTLE_MS,
        board::OUTPUT_SWITCH_REQUIRE_LOAD,
        board::OUTPUT_SWITCH_MIN_MA,
        board::OUTPUT_SWITCH_REQUIRE_INA,
        board::OUTPUT_DISCHARGE_MS,
        board::OUTPUT_CONV_OFF_MS
    );

    spawner.spawn(led_task(app_state, led_ch)).unwrap();
    spawner.spawn(ui_task(app_state, ui_bus)).unwrap();

    let mut t_adc = Instant::now();
    let mut t_supply = Instant::now();
    let mut t_input = Instant::now();
    let mut t_pd = Instant::now();
    // Bring-up ISMON diagnostic cadence (see the `isense:` log below).
    let mut t_isense_log = Instant::now();
    // PD poll period. Backed off to a slow retry while the bus is not answering.
    let mut pd_period = Duration::from_millis(board::INA228_POLL_MS);

    loop {
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
            // The ISMON channel scales with the CC sense range, so read the range
            // the supply tick has published. A short uncontended lock here rather
            // than holding `APP_STATE` across the blocking ADC reads below.
            let cc_range = app_state.lock().await.supply.cc_range;
            let raw = sense.sample(
                &mut adc1,
                &mut adc2,
                &mut adc5,
                &mut pin_vout,
                &mut pin_isense,
                &mut pin_vbus,
                &mut pin_temp_conv,
                &mut pin_temp_in,
                cc_range,
            );
            let mut app = app_state.lock().await;
            // The ADC filter only owns the output-side channels; carry both
            // INA228 readings across its whole-struct replacement.
            let prev_vin = app.telemetry.vin_mv;
            let prev_iin = app.telemetry.iin_ma;
            let prev_pin = app.telemetry.pin_mw;
            let prev_ina_temp = app.telemetry.ina_temp_c;
            let prev_ina_ok = app.telemetry.ina_ok;
            let prev_out_ina = app.telemetry.out_ina;
            let mut filtered = tele_filter.filter(raw);
            filtered.iin_ma = prev_iin;
            filtered.pin_mw = prev_pin;
            filtered.ina_temp_c = prev_ina_temp;
            filtered.ina_ok = prev_ina_ok;
            filtered.out_ina = prev_out_ina;
            if prev_ina_ok {
                // Prefer the input INA228's bus voltage over the ADC divider.
                filtered.vin_mv = prev_vin;
            }
            app.telemetry = filtered;

            // Bring-up ISMON diagnostic. The raw PA3 count and the node voltage
            // are independent of the scaling constants, so comparing them at 0 A
            // vs a known load shows at a glance whether the LT8390A's monitor is
            // moving and what zero should be (calibrate with the output ON and
            // no load). One count is ~0.806 mV, so the counts-per-amp depends on
            // the active range: ~25 counts/A at ×1, ~2.5 counts/A at ×10.
            if now.duration_since(t_isense_log) >= Duration::from_secs(1) {
                t_isense_log = now;
                let raw = sense.last_i_raw();
                let zero = sense.zero_raw();
                defmt::info!(
                    "isense: raw={} ({} mV) zero={} ({} mV) span={} mV raw_i={} mA filt_i={} mA vout={} mV vset={} mV iset={} mA vin={} mV iin={} mA iout_ina={} mA (ina_ok={}) range={:?}",
                    raw,
                    raw * board::ADC_VREF_MV / 4096,
                    zero,
                    zero * board::ADC_VREF_MV / 4096,
                    raw.saturating_sub(zero) * board::ADC_VREF_MV / 4096,
                    raw.saturating_sub(zero) * board::ADC_VREF_MV / 4096
                        * 1000
                        / app.supply.cc_range.mv_per_a(),
                    app.telemetry.iout_ma,
                    app.telemetry.vout_mv,
                    app.supply.v_set_mv,
                    app.supply.i_set_ma,
                    app.telemetry.vin_mv,
                    app.telemetry.iin_ma,
                    app.telemetry.out_ina.current_ma,
                    app.telemetry.out_ina.ok,
                    app.supply.cc_range
                );
            }
        }

        if now.duration_since(t_supply) >= Duration::from_millis(board::SUPPLY_TICK_MS) {
            t_supply = now;
            let mut app = app_state.lock().await;
            // Sweep first: it may move the setpoint/phase this very tick, and the
            // supply tick below pushes the resulting setpoint to the CV DAC.
            sweep.tick(&mut app);
            supply.tick(
                &mut app,
                &mut dac_cv,
                &mut dac_cc,
                &mut conv_en,
                &mut load_en,
                &mut pwr_g,
                &mut high_gain,
            );
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
                supply.tick(
                &mut app,
                &mut dac_cv,
                &mut dac_cc,
                &mut conv_en,
                &mut load_en,
                &mut pwr_g,
                &mut high_gain,
            );
            }
            pd_mgr.negotiate(&mut tps, &mut i2c, &app).await;

            // Input monitor U9: bus voltage, input current/power, die temp.
            match ina_in.poll(&mut i2c).await {
                Some(r) => {
                    app.telemetry.vin_mv = r.bus_mv;
                    app.telemetry.iin_ma = r.current_ma;
                    app.telemetry.pin_mw = r.power_mw;
                    app.telemetry.ina_temp_c = r.die_temp_c;
                    app.telemetry.ina_ok = true;
                }
                None => {
                    if !ina_in.is_present() {
                        app.telemetry.ina_ok = false;
                        app.telemetry.iin_ma = 0;
                        app.telemetry.pin_mw = 0;
                    }
                }
            }

            // Output monitor U12: the load current and the output bus voltage.
            // Authoritative for display (see `Telemetry::display_*`) and the only
            // evidence that the output switch closed; the ADC stays the fast path
            // the supply tick and the sequencer read.
            match ina_out.poll(&mut i2c).await {
                Some(r) => {
                    app.telemetry.out_ina = OutInaTelemetry {
                        bus_mv: r.bus_mv,
                        current_ma: r.current_ma,
                        power_mw: r.power_mw,
                        die_temp_c: r.die_temp_c,
                        ok: true,
                    };
                }
                None => {
                    if !ina_out.is_present() {
                        app.telemetry.out_ina.ok = false;
                    }
                }
            }

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
