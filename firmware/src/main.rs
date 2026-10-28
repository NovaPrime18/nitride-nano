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
//! # Cross-revision safety (`rev3` gate)
//!
//! The same binary must *run* (not damage) on a rev2 board: rev2 shares almost
//! no MCU pins with rev3, and one overlap is destructive — rev2's encoder-A
//! contact sits where rev3 drives the `High-Gain` mux select. At boot the
//! firmware probes the rev3 signatures (an INA228 answering on the PD-bus pins,
//! the OLED answering on the UI bus, or — on an XT90-only session — the UI bus
//! simply being pulled up) and only takes control of `PB1`/`PB2`/`PB3`/`PB6` when
//! one of them matches; otherwise the four HAL wrappers are `parked()` (pins stay
//! floating inputs, no output can be commanded, and the OLED task is not spawned).
//! See the gate block in `main` for the full analysis, and `BENCH.md` for the
//! old-firmware-on-rev3 / new-firmware-on-rev2 matrix.
//!
//! See `PCB/ECO-rev3-usb-dfu.md` for the ECO this pin map implements.

#![no_std]
#![no_main]

use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_stm32::adc::{Adc, Resolution, SampleTime};
use embassy_stm32::dac::{DacCh1, DacChannel, Value};
use embassy_stm32::exti::ExtiInput;
use embassy_stm32::gpio::{Input, Level, Output, OutputType, Pin, Pull, Speed};
use embassy_stm32::i2c::{Config as I2cConfig, I2c, Master};
use embassy_stm32::mode::Async;
use embassy_stm32::rcc::*;
use embassy_stm32::timer::low_level::CountingMode;
use embassy_stm32::timer::qei::{Qei, QeiPin};
use embassy_stm32::timer::simple_pwm::{PwmPin, SimplePwm};
use embassy_stm32::{bind_interrupts, peripherals, Config, Peri};
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Instant, Timer};
use panic_probe as _;

use nitride_firmware::board;
use nitride_firmware::control::dac_cc::CcDac;
use nitride_firmware::control::dac_cv::CvDac;
use nitride_firmware::control::sequencer::Stage;
use nitride_firmware::control::supply::SupplyController;
use nitride_firmware::control::sweep::SweepController;
use nitride_firmware::drivers::tps26750::Tps26750;
use nitride_firmware::eeprom_workflow::{EepromWorkflow, WorkflowState};
use nitride_firmware::hal::converter_enable::ConverterEnable;
use nitride_firmware::hal::high_gain::HighGain;
use nitride_firmware::hal::load_enable::LoadEnable;
use nitride_firmware::hal::output_switch::OutputSwitch;
use nitride_firmware::pd::manager::PdManager;
use nitride_firmware::preflight::Preflight;
use nitride_firmware::runtime::{APP_STATE, I2C_PD_BUS, I2C_UI_BUS};
use nitride_firmware::sense::adc_sense::{AdcSense, TelemetryFilter};
use nitride_firmware::sense::ina_sense::InaSense;
use nitride_firmware::state::{AppState, EepromUiSnapshot, Fault, MenuScreen, OutInaTelemetry};
use nitride_firmware::ui::input::{AnalogButton, InputEvent, InputHandler};
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

/// Sample a GPIO's idle level twice: with the internal pull-up, then with the
/// internal pull-down. Returns `(with_pull_up, with_pull_down)`.
///
/// The internal pull-down is ~40 kΩ, so it loses to a fitted 4.7–10 kΩ
/// external pull-up — "high with the pull-down" therefore proves an external
/// pull-up is present and connected. A line that reads low even with the
/// internal pull-up is being held down hard (a device, or a solder bridge).
fn sample_idle<P: Pin>(pin: &mut Peri<'_, P>) -> (bool, bool) {
    let with_pull_up = Input::new(pin.reborrow(), Pull::Up).is_high();
    let with_pull_down = Input::new(pin.reborrow(), Pull::Down).is_high();
    (with_pull_up, with_pull_down)
}

/// Edge-triggered movement log for a button read by [`AnalogButton`].
///
/// `InputHandler` only reports *verdict* changes, so a press that lands inside the
/// hysteresis band (or a line that does not move at all) would be invisible. This
/// prints every real movement with its measured level, which separates the three
/// cases: works (level below the press threshold), resistive path (moves but stays
/// above it), and dead (no line at all — the pin is not moving).
fn log_analog_move(tag: &'static str, btn: &AnalogButton, last_mv: &mut u32, pressed: bool) {
    if btn.last_mv().abs_diff(*last_mv) < board::ANALOG_BTN_LOG_DELTA_MV {
        return;
    }
    let (pressed_mv, released_mv) = btn.thresholds_mv();
    defmt::info!(
        "input: {} level {} -> {} mV (pressed={}, thresholds {} / {} mV)",
        tag,
        *last_mv,
        btn.last_mv(),
        pressed,
        pressed_mv,
        released_mv
    );
    *last_mv = btn.last_mv();
}

/// Probe every 7-bit address on a bus and log the ones that ACK.
///
/// Bring-up diagnostic only: it separates "the device is absent" from "the
/// device is at an unexpected address" (an SSD1306 strapped to 0x3D, say) and
/// shows whether the bus can transfer at all. A one-byte write of `0x00` is
/// harmless to every device on these two buses (INA228 CONFIG reset value,
/// EEPROM address pointer, SSD1306 lower-column command). The scan aborts on
/// the first error that is not a plain NACK, so a dead bus costs one timeout
/// instead of 112 — and the PD controller's address is skipped because its
/// register protocol is not a plain write.
async fn scan_i2c(i2c: &mut I2c<'_, Async, Master>, bus: &'static str) {
    let mut found = 0u8;
    for addr in 0x08u8..=0x77 {
        if addr == board::TPS26750_ADDR {
            continue;
        }
        match i2c.write(addr, &[0]).await {
            Ok(()) => {
                defmt::info!("boot: {} scan: ACK at 0x{:02x}", bus, addr);
                found = found.saturating_add(1);
            }
            Err(embassy_stm32::i2c::Error::Nack) => {}
            Err(e) => {
                defmt::info!(
                    "boot: {} scan: aborted at 0x{:02x} ({}) - the bus cannot complete a transfer",
                    bus,
                    addr,
                    nitride_firmware::drivers::i2c_error_label(e)
                );
                return;
            }
        }
    }
    if found == 0 {
        defmt::info!("boot: {} scan: no device acknowledged", bus);
    }
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
    // The VBUS divider is 200 k / 10 k, i.e. a high source impedance: give the
    // sample-and-hold a long window so the reading is not dragged by the charge
    // it had before (the only channel on ADC2, so the extra time is free).
    adc2.set_sample_time(SampleTime::CYCLES640_5);
    let mut adc5 = Adc::new(p.ADC5);
    adc5.set_resolution(Resolution::BITS12);
    // Long sample time: VREFINT needs >= 4 us and the NTC divider is high
    // impedance; the extra ~10 us per channel is nothing against the 2 ms tick.
    adc5.set_sample_time(SampleTime::CYCLES640_5);

    // -------------------------------------------------------------------
    // Bring-up self-test: ADC/DAC reference
    // -------------------------------------------------------------------
    // VREF+ (pin 20) and VDDA (pin 21) are tied to +3V3, so the *internal*
    // 1.212 V reference channel must read ≈ 1.212/3.3 × 4096 ≈ 1500 counts.
    // This is the one measurement that separates "the analogue pins are
    // floating/broken" from "*every* channel is railed because the ADC has no
    // valid reference" — on a hand-soldered board the latter usually means a
    // cold joint on pin 20/21 (or on a VSSA/VDDA neighbour), and it also means
    // both DACs are commanding garbage, so the converter can never regulate.
    // The internal temperature sensor is sampled as a second, independent
    // analogue-path sanity check (it must land between the rails).
    let (vrefint_raw, vrefint5_raw, ts_raw) = {
        let mut vrefint = adc1.enable_vrefint();
        let vrefint_raw = adc1.blocking_read(&mut vrefint);
        let mut temp_sense = adc1.enable_temperature();
        let ts_raw = adc1.blocking_read(&mut temp_sense);
        // The same internal reference on ADC5. Three ADC instances rail on this
        // board (ADC1/ADC2/ADC5 through their external pins); if ADC5's *internal*
        // channel is healthy while ADC1's is railed, ADC1 itself is damaged. If
        // both rail, the shared analogue supply/ground (VREF+, VDDA, EP/VSSA) is
        // the common cause — which a DMM can still read as a fine 3.3 V on a
        // high-impedance joint that collapses under the sampling charge.
        let mut vrefint5 = adc5.enable_vrefint();
        let vrefint5_raw = adc5.blocking_read(&mut vrefint5);
        defmt::info!(
            "boot: adc self-test: VREFINT raw: ADC1={} ADC5={} (expect ~1500 each with VREF+ = 3.3 V), internal TS(ADC1) raw={} (must be between the rails)",
            vrefint_raw,
            vrefint5_raw,
            ts_raw
        );
        if !(1_200..=1_800).contains(&vrefint_raw) {
            defmt::warn!(
                "boot: VREFINT OUT OF RANGE - VREF+ (pin 20) / VDDA (pin 21) is not at 3.3 V or is not connected; every analogue reading AND both DAC outputs are invalid until this is fixed (check those joints, C4/C5 decoupling and the +3V3 rail)"
            );
        }
        if ts_raw == 0 || ts_raw >= 4095 {
            defmt::warn!(
                "boot: internal temperature sensor is railed too - confirms the ADC does not have a usable reference"
            );
        }
        if !(1_200..=1_800).contains(&vrefint5_raw) && (1_200..=1_800).contains(&vrefint_raw) {
            defmt::warn!(
                "boot: ADC1 and ADC5 disagree on the internal reference - one ADC instance is damaged rather than the shared analogue supply"
            );
        }
        (vrefint_raw, vrefint5_raw, ts_raw)
    };

    let mut pin_vout = p.PA0;
    let mut pin_temp_conv = p.PA1;
    let mut pin_isense = p.PA3;
    let mut pin_vbus = p.PA7;
    let mut pin_temp_in = p.PA9;

    defmt::timestamp!("{=u64:us}", { embassy_time::Instant::now().as_micros() });

    // The rev3 power-path pins (PB1/PB3/PB2/PB6) are created *after* the
    // board-detect probe further down, so that a board which is not a rev3
    // never has any of them driven — see the `rev3` gate block below for why
    // that matters on rev2 hardware.

    // `BUTT_1` (PB9) is normally a plain GPIO, but PB9 has no analog input and a
    // rev3 board has been measured holding it at ~0.8 V *released* (≈13 kΩ to
    // ground on the pin), which a GPIO reads as permanently pressed. When that pin
    // is reworked onto PA2 (`ADC1_IN3`) instead, `board::BTN1_ANALOG` selects the
    // analog path — see that constant for the rework and the thresholds.
    let mut btn1_gpio = None;
    if board::BTN1_ANALOG {
        // PB9 is left disconnected by the rework that moves the read to PA2: park
        // it as a pulled-down input so the pad does not float.
        let _ = Input::new(p.PB9, Pull::Down);
    } else {
        btn1_gpio = Some(Input::new(p.PB9, Pull::Up));
    }
    let mut pin_btn1_adc = p.PA2;
    let mut btn1_adc = AnalogButton::new(board::BTN1_PRESSED_MV, board::BTN1_RELEASED_MV);
    let btn2 = Input::new(p.PB10, Pull::Up);
    let btn3 = Input::new(p.PB11, Pull::Up);
    // rev3: the encoder button moved to PB0 to free PB4 for TIM3_CH1.
    //
    // PB0 is *not* a digital input here: rev3 boards measure ~2.4 V on `ENC_BUTT`
    // when the switch is pressed (a resistive press path — see
    // `board::ENC_BTN_PRESSED_MV`), which no GPIO reads as a logic low. It is
    // sampled as ADC1_IN15 instead and thresholded by `AnalogButton`. The external
    // R64 10 kΩ pull-up is what holds the released line at 3.3 V, because analog
    // mode leaves the MCU's internal pull-up off.
    let mut pin_enc_btn = p.PB0;
    let mut enc_btn = AnalogButton::new(board::ENC_BTN_PRESSED_MV, board::ENC_BTN_RELEASED_MV);

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
    // JP4/JP5 are bridged (pads 2-3, the `I2CC_*_EEPROM` to `I2CC_*_MCU`
    // pair). rev3 moved this from I2C3 (PA8/PB5) to free PA8 for the PD bus
    // and PB5 for the encoder; the R88/R89 10 kΩ pull-ups live on the MCU-side
    // nets, so the bus stays defined even with the OLED socket empty.
    //
    // Before the peripherals claim the pins, sample each bus line's idle state
    // with the MCU's internal pull-up then pull-down. The internal pull-down is
    // ~40 kΩ, so it loses to a fitted 4.7–10 kΩ external pull-up: "high with
    // pull-down" proves the external pull-up is present and connected, while
    // "low with pull-up" means the line is held low by a device or a solder
    // bridge. This is the fastest way to tell a missing pull-up joint from a
    // shorted line on a hand-soldered board.
    let mut pin_ui_scl = p.PC6;
    let mut pin_ui_sda = p.PB7;
    let mut pin_pd_scl = p.PC4;
    let mut pin_pd_sda = p.PA8;
    let (ui_scl_pu, ui_scl_pd, ui_sda_pu, ui_sda_pd, pd_scl_pu, pd_scl_pd, pd_sda_pu, pd_sda_pd);
    {
        let (a, b) = sample_idle(&mut pin_ui_scl);
        let (c, d) = sample_idle(&mut pin_ui_sda);
        let (e, f) = sample_idle(&mut pin_pd_scl);
        let (g, h) = sample_idle(&mut pin_pd_sda);
        (ui_scl_pu, ui_scl_pd, ui_sda_pu, ui_sda_pd, pd_scl_pu, pd_scl_pd, pd_sda_pu, pd_sda_pd) =
            (a, b, c, d, e, f, g, h);
        defmt::info!(
            "boot: bus idle levels: UI SCL(PC6) pu={} pd={} SDA(PB7) pu={} pd={} | PD SCL(PC4) pu={} pd={} SDA(PA8) pu={} pd={} (pd=1 => external pull-up present; pu=0 => line held low)",
            ui_scl_pu,
            ui_scl_pd,
            ui_sda_pu,
            ui_sda_pd,
            pd_scl_pu,
            pd_scl_pd,
            pd_sda_pu,
            pd_sda_pd
        );
        if !ui_scl_pd || !ui_sda_pd {
            defmt::warn!(
                "boot: UI bus pull-up missing on {} - check R88/R89 (10 kΩ to +3V3) and the OLED/EEPROM sockets",
                if !ui_scl_pd && !ui_sda_pd { "SCL and SDA" } else if !ui_scl_pd { "SCL" } else { "SDA" }
            );
        }
        if !ui_scl_pu || !ui_sda_pu {
            defmt::warn!(
                "boot: UI bus line held LOW even with the internal pull-up - solder bridge / short to GND on {}",
                if !ui_scl_pu && !ui_sda_pu { "SCL and SDA" } else if !ui_scl_pu { "SCL (PC6)" } else { "SDA (PB7)" }
            );
        }
        if !pd_scl_pd || !pd_sda_pd {
            defmt::info!(
                "boot: PD bus pull-ups not powered (expected with no USB-C VBUS: R31/R32 sit on the TPS26750's +3V3_LDO)"
            );
        }
    }
    let i2c_ui = I2c::new(
        p.I2C4, pin_ui_scl, pin_ui_sda, Irqs, p.DMA1_CH3, p.DMA1_CH4, i2c_ui_config,
    );
    let ui_bus = I2C_UI_BUS.init(Mutex::new(i2c_ui));

    // PD bus (I2C2): TPS26750 USB-PD controller, INA228 input monitor (U9) and
    // INA228 output monitor (U12). rev3 moved this from I2C1 (PB8/PB7) to free
    // PB8 for the BOOT0 strap; PC4/PA8 are a valid I2C2 pin pair, so the rev2
    // SCL bodge is gone.
    let i2c_pd = I2c::new(
        p.I2C2, pin_pd_scl, pin_pd_sda, Irqs, p.DMA1_CH1, p.DMA1_CH2, i2c_pd_config,
    );
    let pd_bus = I2C_PD_BUS.init(Mutex::new(i2c_pd));
    defmt::info!("boot: i2c up");

    let app = AppState::default();
    let app_state = APP_STATE.init(Mutex::new(app));

    let mut sweep = SweepController::new();
    let mut sense = AdcSense::new();
    let mut input = InputHandler::new();
    let mut tele_filter = TelemetryFilter::new();
    let mut pd_mgr = PdManager::new();
    let mut tps = Tps26750::new(board::TPS26750_ADDR);
    let mut ina_in = InaSense::new(board::INA228_IN_ADDR, board::INA228_IN_SHUNT_MOHM);
    // The output monitor carries the one-time DMM cross-check factor: its
    // VBUS is what the CV servo regulates against and what the calibration
    // fit runs on (see board::INA228_OUT_VBUS_SCALE_*).
    let mut ina_out = InaSense::new_scaled(
        board::INA228_OUT_ADDR,
        board::INA228_OUT_SHUNT_MOHM,
        board::INA228_OUT_VBUS_SCALE_NUM,
        board::INA228_OUT_VBUS_SCALE_DEN,
    );
    let mut eeprom_workflow = EepromWorkflow::new();

    // -----------------------------------------------------------------------
    // Board-revision detect + the rev3 power-path gate
    // -----------------------------------------------------------------------
    //
    // The firmware must also *run* (not just survive) on a rev2 board, and it
    // must never damage one. Four pins make that a real requirement:
    //
    // * **PB6** is rev3's `High-Gain` mux select, but rev2's **encoder A**
    //   signal: 10 kΩ pull-up with a switch contact to ground. Driving it high
    //   while the contact is closed (~half of all knob positions) short-circuits
    //   a push-pull STM32 output at ~4× its absolute-maximum source current.
    // * **PB1/PB2/PB3** are unconnected on rev2 — driving them is pointless.
    // * rev2's converter-disable line is **PA11**, which rev3 uses for
    //   `USB_D_N`; leaving it alone on rev2 leaves that converter enabled with
    //   the parked CV DAC, i.e. regulating toward 0 V — safe, but it means the
    //   firmware cannot be allowed to blind-enable an output it cannot
    //   supervise (rev2's load switch is the chip-driven `LOADTG`, which
    //   closes by itself as soon as VOUT reaches ~2.4–3 V).
    //
    // So: probe the rev3 signature, and only then take control of the rev3-only
    // pins. There are **three** signatures, and none can fire on a rev2 board:
    //
    // 1. *any* INA228 answering on the PD bus (I2C2: PC4/PA8) — U9 @0x40 or
    //    U12 @0x44. rev2 carries no device on those pins (PC4 is rev2's
    //    unconnected/PD-SCL pin, PA8 is rev2's OLED SCL), so nothing can ACK.
    // 2. the **SSD1306 answering on the UI bus** (I2C4: PC6/PB7, 0x3C). rev2's
    //    PC6 is unconnected with no pull-up, so the bus cannot even clock.
    // 3. the **UI bus being pulled up at all** (`R88`/`R89` present), which is a
    //    static property of the board rather than of a device answering.
    //
    // Signature 2 exists because signature 1 depends on a *rail the PD
    // controller gates*: `+3V3_LDO` is the TPS26750's own internal LDO output
    // (U4 pin 1) and it powers the MCU-side PD-bus pull-ups (`R31`/`R32`).
    // With no USB-C VBUS the TPS is unpowered, `+3V3_LDO` is dead, and I2C2
    // cannot acknowledge *anything* — on a perfectly healthy rev3 too. The UI
    // bus pull-ups (`R88`/`R89`) are on the always-on `+3V3`, so the OLED
    // still answers in that state. Without this second signature an
    // XT90-powered bench session (no USB-C attached) would park a good board's
    // whole power path.
    //
    // Signature 3 is the backstop for exactly that session: on XT90 signature 2
    // is the only evidence available, and a panel that is still in its power-on
    // reset at that instant simply does not answer. Signatures 2 and 3 are
    // therefore tried from the same bus state, and 3 cannot be lost to a race.
    //
    // On a rev3 whose *both* power monitors are absent, the INA probe fails but
    // the OLED usually answers, so the board is still recognised — and an
    // enable then runs the sequencer until `SwitchOn`, where
    // `OUTPUT_SWITCH_REQUIRE_INA` refuses to close the load switch (see
    // BENCH.md §4 for the INA-less bench flags). On a genuinely unknown board
    // nothing answers and the path stays parked, which is the conservative
    // outcome.
    //
    // A few fast retries absorb a slow power-up of the monitors; the ordinary
    // 1 s absent-retry cadence would turn a single boot-time glitch into a
    // misdetect.
    // The PD bus only exists while a USB-C source supplies VBUS (its pull-ups
    // hang off the TPS26750's own `+3V3_LDO`), so probe it only when the
    // idle-level check proved the pull-ups are powered. Skipping otherwise saves
    // ~45 ms of boot and, more importantly, eight pointless timeout warnings on
    // every XT90-only session — XT90 is a supported input, not a fault.
    let pd_bus_powered = pd_scl_pd || pd_sda_pd;
    let mut ina_detected = false;
    if pd_bus_powered {
        for attempt in 0..5u8 {
            let mut i2c = pd_bus.lock().await;
            ina_in.arm_immediate_probe();
            ina_out.arm_immediate_probe();
            let in_ok = ina_in.init(&mut i2c).await;
            let out_ok = ina_out.init(&mut i2c).await;
            drop(i2c);
            if in_ok || out_ok {
                ina_detected = true;
                break;
            }
            if attempt < 4 {
                Timer::after(Duration::from_millis(5)).await;
            }
        }
    } else {
        defmt::info!(
            "boot: no PD bus (I2C2 pull-ups unpowered) - XT90/DC input mode; skipping the PD-bus probes. Attach a USB-C source to bring the TPS26750 and both INA228s up"
        );
    }

    // Signature 2, only when the PD bus could not answer. The write is 0x00 —
    // the SSD1306's (reset-value) lower-column command, harmless; only the ACK
    // matters. On a rev2 board this costs the UI-bus timeout once at boot.
    //
    // On an **XT90-only session this probe is the only rev3 signature there
    // is**, and a panel that is still inside its own power-on reset answers
    // nothing while it is coming up. A single unlucky sample therefore used to
    // park a perfectly healthy board — no power path *and* no UI task, i.e. a
    // permanently dark OLED with a working LED until the user pressed reset and
    // gave the panel a second chance. That is the "OLED only comes up on USB-C"
    // report: with USB-C attached the five INA228 probe rounds above take long
    // enough that the panel is always ready by the time this runs.
    //
    // So retry, but only on a bus that has pull-ups: there a retry is cheap and
    // meaningful, while a bus with no pull-ups cannot answer at all (rev2 leaves
    // PC6/PB7 unconnected), which turns five timeouts into one.
    let ui_bus_pulled_up = ui_scl_pd || ui_sda_pd;
    let probe_tries: u8 = if ui_bus_pulled_up { 5 } else { 1 };
    let mut oled_ack = false;
    if !ina_detected {
        for attempt in 0..probe_tries {
            let mut i2c = ui_bus.lock().await;
            let res = i2c.write(board::SSD1306_ADDR, &[0x00]).await;
            drop(i2c);
            match res {
                Ok(()) => {
                    if attempt > 0 {
                        defmt::info!(
                            "boot: OLED answered on attempt {}/{} - the panel was still in power-on reset at first contact",
                            attempt + 1,
                            probe_tries
                        );
                    }
                    oled_ack = true;
                    break;
                }
                Err(e) => {
                    if attempt + 1 == probe_tries {
                        defmt::info!(
                            "boot: no OLED at 0x{:02x} on the UI bus ({}, {} attempt(s))",
                            board::SSD1306_ADDR,
                            nitride_firmware::drivers::i2c_error_label(e),
                            probe_tries
                        );
                    }
                }
            }
            if attempt + 1 < probe_tries {
                Timer::after(Duration::from_millis(10)).await;
            }
        }
    }
    // Signature 3 (XT90 only): the rev3 UI bus is fitted and pulled up (`R88`/
    // `R89` to the always-on `+3V3`) whether or not a display is plugged in,
    // while rev2 leaves those two pins unconnected — the rev2 netlist has a net
    // literally named `unconnected-(U2-PC6-Pad29)`, with no pull-up to beat the
    // ~40 kΩ internal pull-down. That makes this a *static* signature, so unlike
    // the ACK above it cannot lose a race against a cold panel. It is only
    // consulted when the PD bus is unpowered; with USB-C present the INA228s
    // already answer and this is redundant. Both lines must show their pull-up:
    // this decision is what allows `PB6` to be driven, and one noisy floating pin
    // must never be enough for that.
    let rev3 = ina_detected || oled_ack || (ui_scl_pd && ui_sda_pd && !pd_bus_powered);

    // One boot-time reading of the encoder switch, so the bench can see both ends
    // of the detection window before pressing anything: released should read
    // ~3300 mV (the 3V3 rail through R64), and a press has to fall below
    // `board::ENC_BTN_PRESSED_MV`. A rev3 board that measures only ~2400 mV
    // pressed is exactly why this input is analog — see that constant.
    let mut enc_boot_mv = 0u32;
    let mut btn1_boot_mv = 0u32;
    if rev3 {
        enc_boot_mv = adc1.blocking_read(&mut pin_enc_btn) as u32 * board::ADC_VREF_MV / 4096;
        defmt::info!(
            "boot: encoder button PB0 (ADC1_IN15) at {} mV - pressed <= {} mV, released >= {} mV",
            enc_boot_mv,
            board::ENC_BTN_PRESSED_MV,
            board::ENC_BTN_RELEASED_MV
        );
        if board::BTN1_ANALOG {
            btn1_boot_mv = adc1.blocking_read(&mut pin_btn1_adc) as u32 * board::ADC_VREF_MV / 4096;
            defmt::info!(
                "boot: BUTT_1 on PA2 (ADC1_IN3, reworked) at {} mV - pressed <= {} mV, released >= {} mV",
                btn1_boot_mv,
                board::BTN1_PRESSED_MV,
                board::BTN1_RELEASED_MV
            );
        } else if btn1_gpio.as_ref().is_some_and(|p| p.is_low()) {
            // PB9 low at boot: either the button is held, or the pin never reaches
            // V_IL's high side — a rev3 board has been measured at ~0.8 V *released*,
            // i.e. ~13 kΩ to ground on the pad, which a GPIO calls "pressed". Say so
            // once here instead of letting the first `input:` line look like a press,
            // and name both fixes.
            defmt::warn!(
                "boot: BUTT_1 (PB9) reads low with the button untouched - the pin is stuck below V_IL (~1 V), so the button is dead in firmware. Check the PB9 pad (pin 47) against its neighbours (46/48) and R66; if the pad cannot be repaired, wire the MCU side of R66 to PA2 (ADC1_IN3) and set board::BTN1_ANALOG = true"
            );
        }
    }
    defmt::info!(
        "boot: board detect: INA228 U9@0x{:02x}={} U12@0x{:02x}={} | OLED@0x{:02x} ack={} | UI bus pull-ups={} -> rev3 power path {}",
        board::INA228_IN_ADDR,
        ina_in.is_present(),
        board::INA228_OUT_ADDR,
        ina_out.is_present(),
        board::SSD1306_ADDR,
        oled_ack,
        ui_bus_pulled_up,
        if rev3 { "ACTIVE" } else { "PARKED (PB1/PB2/PB3/PB6 never driven; output enable refused)" }
    );
    if rev3 && !oled_ack {
        defmt::warn!(
            "boot: rev3 recognised without the display answering - the UI task will keep retrying once the panel is up (check J5 and the module orientation if it never does)"
        );
    }
    if pd_bus_powered && !ina_detected {
        defmt::warn!(
            "boot: PD bus is powered but nothing answered - check the TPS26750/INA228 joints and the I2C2 lines (PC4/PA8)"
        );
    }
    // Address scans: list what actually answers on each bus. Diagnostic only —
    // it separates "device absent" from "device at an unexpected address"
    // (e.g. an SSD1306 strapped to 0x3D) and shows whether a bus is alive at
    // all. The scan aborts on the first non-NACK error, so a dead bus costs one
    // timeout rather than 112.
    if pd_bus_powered {
        let mut i2c = pd_bus.lock().await;
        scan_i2c(&mut i2c, "PD(I2C2)").await;
    }
    {
        let mut i2c = ui_bus.lock().await;
        scan_i2c(&mut i2c, "UI(I2C4)").await;
    }
    // Let the host drain the log buffer before the boot banner burst: the
    // banner is a dozen long frames and the RTT up-buffer is small, so one
    // burst can drop frames (it did once, hiding this very line).
    Timer::after(Duration::from_millis(25)).await;

    // rev3 power path. Park all three in their safe state before anything else
    // can enable the converter: chip disabled, `CNV-EN-LOADEN` low (bridge
    // parked, output node isolated) and the load switch open. `PWR_G` has a
    // 100 kΩ pulldown on the board, so an MCU held in reset already leaves the
    // load disconnected. On a non-rev3 board these are the `parked()`
    // constructors: the pins stay in their reset floating-input state and
    // every `set_*` call is a no-op.
    let (mut conv_en, mut load_en, mut pwr_g, mut high_gain) = if rev3 {
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
        // TMUX1574's internal SEL pull-down already selects while it floats)
        // and then follows the setpoint; `SupplyController` only moves it while
        // the stage is parked.
        let high_gain = HighGain::new(
            Output::new(p.PB6, Level::Low, Speed::Low),
            board::CcRange::LowGain,
        );
        (conv_en, load_en, pwr_g, high_gain)
    } else {
        (
            ConverterEnable::parked(),
            LoadEnable::parked(),
            OutputSwitch::parked(),
            HighGain::parked(),
        )
    };

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
        "boot: cv map: code0 -> {} mV, code{} -> {} mV (VREF {} mV, R19 {} R20 {} R36 {}) [map: {}]",
        CvDac::vout_at_code_zero_mv(),
        board::DAC_MAX_CODE,
        CvDac::code_to_vout_mv(board::DAC_MAX_CODE),
        board::DAC_VREF_MV,
        board::CV_FB_TOP_OHM,
        board::CV_FB_BOTTOM_OHM,
        board::CV_SUM_OHM,
        CvDac::map_label()
    );
    defmt::info!(
        "boot: cv cal valid={} A={} mV B={} uv/lsb; u12 vbus scale {}/{}; servo enabled={} period={} ms deadband={} mV max_trim={} lsb",
        board::CV_CAL_VALID,
        board::CV_CAL_A_MV,
        board::CV_CAL_B_UV_PER_LSB,
        board::INA228_OUT_VBUS_SCALE_NUM,
        board::INA228_OUT_VBUS_SCALE_DEN,
        board::CV_SERVO_ENABLE,
        board::CV_SERVO_PERIOD_MS,
        board::CV_SERVO_DEADBAND_MV,
        board::CV_SERVO_MAX_TRIM_LSB
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
    // Drain the RTT buffer again: the two `cc range` lines above are the
    // longest frames in the banner (~20 arguments each).
    Timer::after(Duration::from_millis(25)).await;
    defmt::info!(
        "boot: high-gain switches at iset <= {} mA; UI span {}..{} mA",
        board::HIGH_GAIN_MAX_SET_MA,
        board::CC_SET_MIN_MA,
        board::CC_SET_MAX_MA
    );
    if let Some(range) = board::FORCE_CC_RANGE {
        defmt::warn!(
            "boot: CC range PINNED to {:?} by board::FORCE_CC_RANGE - the setpoint no longer selects the range (only do this with the other range's front end physically disabled, e.g. U7 S1A/S2A lifted); the settable span is {}..{} mA",
            range,
            board::CC_SET_MIN_MA,
            board::CC_SET_MAX_MA
        );
    }
    defmt::info!(
        "boot: rev3 load-switch interlock: PWR_G held closed while vout > {} mV; early close={}, disable hold <= {} ms, safe-open {} mA; sense watchdog: > {} mW in with < {}% of it at the output, {} ms confirm",
        board::OUTPUT_SWITCH_SAFE_VOUT_MV,
        board::OUTPUT_SWITCH_EARLY_CLOSE,
        board::OUTPUT_SWITCH_HOLD_MAX_MS,
        board::OUTPUT_SWITCH_SAFE_OPEN_MA,
        board::SENSE_OPEN_MIN_PIN_MW,
        board::SENSE_OPEN_RATIO_PCT,
        board::SENSE_OPEN_CONFIRM_MS
    );
    defmt::info!(
        "boot: iout software trip margin {}%; supply snapshot every {} ms",
        board::IOUT_TRIP_MARGIN_PCT,
        board::SUPPLY_LOG_MS
    );
    // The rev3 power-up sequence, so the bench can check the scope against these
    // exact numbers. See `board::OUTPUT_*` and `control::sequencer`.
    defmt::info!(
        "boot: output seq: preloaden {} ms, ramp timeout {} ms (reached {}%), switch settle {} ms + verify window {} ms, require_load={} (min {} mA), require_ina={}, discharge {} ms, conv-off {} ms",
        board::OUTPUT_PRELOADEN_MS,
        board::OUTPUT_RAMP_TIMEOUT_MS,
        board::VOUT_REACHED_PCT,
        board::OUTPUT_SWITCH_SETTLE_MS,
        board::OUTPUT_SWITCH_VERIFY_MS,
        board::OUTPUT_SWITCH_REQUIRE_LOAD,
        board::OUTPUT_SWITCH_MIN_MA,
        board::OUTPUT_SWITCH_REQUIRE_INA,
        board::OUTPUT_DISCHARGE_MS,
        board::OUTPUT_CONV_OFF_MS
    );

    // -----------------------------------------------------------------------
    // First-enable interlock (`src/preflight.rs`)
    // -----------------------------------------------------------------------
    // Everything below runs with the output stage parked and is purely
    // observational: it reads the driven gate-line levels back, takes one raw
    // ADC sample, probes the PD controller, and then lets the self-test decide
    // whether the converter may be enabled at all. Nothing here energises the
    // converter.
    let preflight_ok = if rev3 {
        let (pb1, pb2, pb3, pb6) = board::power_path_pin_levels();
        let raw = sense.sample(
            &mut adc1,
            &mut adc2,
            &mut adc5,
            &mut pin_vout,
            &mut pin_isense,
            &mut pin_vbus,
            &mut pin_temp_conv,
            &mut pin_temp_in,
            board::CcRange::LowGain,
        );
        let tps_present = if pd_bus_powered {
            let mut i2c = pd_bus.lock().await;
            tps.init(&mut i2c).await
        } else {
            // No pull-ups on I2C2: probing would only cost a timeout and a WARN.
            false
        };
        let adc_ok =
            (1_200..=1_800).contains(&vrefint_raw) && (1_200..=1_800).contains(&vrefint5_raw);
        let optr = embassy_stm32::pac::FLASH.optr().read();

        let mut pf = Preflight::new(pd_bus_powered);
        pf.adc_reference(vrefint_raw, vrefint5_raw, ts_raw);
        pf.thermistors(raw.temp_conv_c, raw.temp_input_c);
        pf.power_path_pins(pb1, pb2, pb3, pb6);
        pf.input_monitor(ina_in.is_present());
        pf.output_monitor(ina_out.is_present());
        pf.pd_controller(tps_present);
        // Only trust the ADC-derived VIN when the reference itself is sane,
        // otherwise the railed reading would add a bogus "input too high".
        pf.input_voltage(raw.vin_mv, adc_ok);
        pf.output_parked(raw.vout_mv);
        pf.display(oled_ack);
        pf.option_bytes(Some((optr.n_swboot0(), optr.n_boot1())));
        pf.finish()
    } else {
        // Non-rev3 board: the rev3-only pins are never driven and the revision
        // gate parks the whole power path anyway, so only the reference check
        // is meaningful. Report it, but leave the enable gate open — the
        // revision gate is what refuses here.
        let mut pf = Preflight::new(pd_bus_powered);
        pf.adc_reference(vrefint_raw, vrefint5_raw, ts_raw);
        let _ = pf.finish();
        defmt::info!(
            "preflight: hardware checks skipped (not a rev3 board) - the revision gate parks the output path"
        );
        true
    };
    if preflight_ok {
        defmt::info!(
            "preflight: first-enable recipe: no load attached, Vset 5 V, Iset 1 A, then press BTN2 - expect PRE -> RMP -> SWO -> ON, with PB1 low, then PB3 high, and PB2 closing only after the ramp"
        );
    }
    let mut supply = SupplyController::with_gates(rev3, preflight_ok);
    if !preflight_ok {
        // Latch it now so the OLED header (SELF-TEST) and the LED code 14 show
        // the state immediately, instead of only when BTN2 is pressed.
        app_state.lock().await.supply.fault = Fault::PreflightFailed;
    }

    spawner.spawn(led_task(app_state, led_ch)).unwrap();
    // The OLED task owns OLED *initialisation* and re-probing, so it runs
    // whenever the board is recognised: `rev3` already covers the case where
    // only the OLED answered (an INA-less or USB-C-less bench session) and the
    // case where the XT90 UI-bus signature fired without the display answering
    // — the panel then gets its retries here instead of a dead board. On a board
    // that matched no signature it is not spawned, which keeps a rev2 from being
    // stalled by per-transaction timeouts on a bus it cannot answer.
    if rev3 {
        spawner.spawn(ui_task(app_state, ui_bus)).unwrap();
    }

    let mut t_adc = Instant::now();
    let mut t_supply = Instant::now();
    let mut t_input = Instant::now();
    let mut t_pd = Instant::now();
    // Bring-up ISMON diagnostic cadence (see the `isense:` log below).
    let mut t_isense_log = Instant::now();
    // Last raw button/encoder-switch levels, for the edge-triggered `input:`
    // diagnostic below. `true` = released (active-low inputs).
    let mut last_btn_levels = (true, true, true, true);
    // Last encoder-switch level that went into the log, so any real movement of
    // the analog line is reported once (see `board::ANALOG_BTN_LOG_DELTA_MV`).
    // Seeded with the boot reading so the first poll does not log a spurious
    // "0 -> 3292".
    let mut enc_last_logged_mv = enc_boot_mv;
    // Same bookkeeping for `BUTT_1` on its optional analog path.
    let mut btn1_last_logged_mv = btn1_boot_mv;
    // Last raw ADC reading of the input divider, kept separately because the
    // input monitor overwrites `telemetry.vin_mv` with its own (better) value
    // whenever it is available. The divider cross-check below compares the two.
    let mut adc_vin_mv: u32 = 0;
    // Last logged cross-check ratios (percent); 0 = not logged yet. The lines are
    // edge-triggered so a steady bench session does not repeat them.
    let mut xchk_vin_pct: u32 = 0;
    let mut xchk_vout_pct: u32 = 0;
    // PD poll period. Backed off to a slow retry while the bus is not answering;
    // with no PD bus at all (XT90/DC input) start slow so the boot log stays
    // clean, then keep a 10 s self-heal probe in the loop above.
    let mut pd_period = if pd_bus_powered {
        Duration::from_millis(board::INA228_POLL_MS)
    } else {
        Duration::from_secs(10)
    };

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

            // `BUTT_1`: a GPIO unless the pin has been reworked onto PA2, in which
            // case the same ADC-threshold treatment as the encoder switch applies.
            let btn1_pressed = if board::BTN1_ANALOG {
                let raw = adc1.blocking_read(&mut pin_btn1_adc) as u32;
                btn1_adc.sample(raw * board::ADC_VREF_MV / 4096)
            } else {
                btn1_gpio.as_ref().is_some_and(|p| p.is_low())
            };

            // The encoder switch is analog (see `AnalogButton`): convert its level
            // to a pressed/released verdict before the debounce.
            let enc_raw = adc1.blocking_read(&mut pin_enc_btn) as u32;
            let enc_mv = enc_raw * board::ADC_VREF_MV / 4096;
            // Only rev3 has its encoder switch on PB0; rev2 leaves PB0
            // unconnected (`unconnected-(U2-PB0-Pad17)` in the rev2 netlist) and
            // routed the switch to PB4, which this pin map cannot read. Gating
            // the verdict keeps a floating analog input there from inventing
            // phantom clicks. The sample is still taken so `enc_mv` stays live in
            // the diagnostic below.
            let enc_pressed = rev3 && enc_btn.sample(enc_mv);

            // Raw-level diagnostic: log whenever any button or the encoder switch
            // changes level, *before* debouncing. That separates a dead input
            // (pin never moves — check the 1 kΩ series resistor and the switch's
            // ground return) from a dead handler/UI (level moves, nothing reacts).
            // `enc_mv` is the encoder switch's live analog level: ~3300 released,
            // and whatever the press path actually produces when pressed (rev3
            // has measured ~2400, which is why the threshold is analog).
            let levels = (!btn1_pressed, btn2.is_high(), btn3.is_high(), !enc_pressed);
            if levels != last_btn_levels {
                last_btn_levels = levels;
                defmt::info!(
                    "input: btn1={} btn2={} btn3={} enc_btn={} (1=released, 0=pressed) enc_mv={} qei={}",
                    levels.0 as u8,
                    levels.1 as u8,
                    levels.2 as u8,
                    levels.3 as u8,
                    enc_btn.last_mv(),
                    c
                );
            }

            // Level-movement diagnostic for every analog button: the line above
            // only prints when the *verdict* flips, so a press that lands between
            // the two thresholds (or a switch that does not move the line at all)
            // would be invisible. Printing any real movement with its measured
            // level separates the three failure modes:
            //   movement logged, below threshold  -> the switch works, tune the
            //                                        constant;
            //   movement logged, stays >= threshold -> resistive press path;
            //   no line at all while pressing     -> the pin is not moving: open
            //                                        contact/joint, not a threshold.
            if rev3 {
                log_analog_move(
                    "encoder PB0",
                    &enc_btn,
                    &mut enc_last_logged_mv,
                    enc_pressed,
                );
            }
            if board::BTN1_ANALOG {
                log_analog_move(
                    "btn1 PA2",
                    &btn1_adc,
                    &mut btn1_last_logged_mv,
                    btn1_pressed,
                );
            }

            input.poll(btn1_pressed, &btn2, &btn3, enc_pressed, detents);
            if let Some(ev) = input.last_event {
                let mut app = app_state.lock().await;
                let previous_screen = app.ui.screen;
                if app.ui.screen == MenuScreen::EepromFlash {
                    eeprom_workflow.handle_input(ev);
                }
                match ev {
                    InputEvent::EncBtn => defmt::info!("input: encoder click"),
                    InputEvent::EncDoubleClick => defmt::info!("input: encoder double click"),
                    _ => {}
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
            adc_vin_mv = raw.vin_mv;
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

            // ---------------------------------------------------------------
            // Divider cross-checks (bring-up diagnostics)
            // ---------------------------------------------------------------
            // An XT90/DC session has no PD-side monitor, so its Vin and Vout come
            // from resistor dividers. Whenever the monitors *are* available they
            // are a transfer reference for those dividers, and these lines turn a
            // divider disagreement into a number plus the exact constant to set.
            if app.telemetry.ina_ok && adc_vin_mv > 1_000 {
                let ina_vin = app.telemetry.vin_mv;
                if ina_vin > 1_000 {
                    let pct = (adc_vin_mv as u64 * 100 / ina_vin as u64) as u32;
                    if xchk_vin_pct == 0 || pct.abs_diff(xchk_vin_pct) >= 5 {
                        xchk_vin_pct = pct;
                        let corrected = (board::VBUS_SENSE_NUM as u64 * ina_vin as u64
                            / adc_vin_mv as u64) as u32;
                        defmt::info!(
                            "supply: vin divider cross-check: adc={} mV vs input monitor={} mV (adc reads {}% of it) -> for the ADC to agree set board::VBUS_SENSE_NUM = {} mV",
                            adc_vin_mv,
                            ina_vin,
                            pct,
                            corrected
                        );
                    }
                }
            }
            // Vsense_REG_OUT sits on the converter side of Q5 and U12's VBUS on
            // the connector side, so the two only measure the same node with the
            // load switch closed.
            if app.telemetry.out_ina.ok
                && app.supply.stage == Stage::Running
                && app.telemetry.vout_mv > 2_000
            {
                let ina_vout = app.telemetry.out_ina.bus_mv;
                if ina_vout > 2_000 {
                    let pct = (app.telemetry.vout_mv as u64 * 100 / ina_vout as u64) as u32;
                    if xchk_vout_pct == 0 || pct.abs_diff(xchk_vout_pct) >= 5 {
                        xchk_vout_pct = pct;
                        let corrected = (board::VOUT_SENSE_NUM as u64 * ina_vout as u64
                            / app.telemetry.vout_mv as u64) as u32;
                        defmt::info!(
                            "supply: vout divider cross-check: adc={} mV vs output monitor={} mV (adc reads {}% of it) -> for the ADC to agree set board::VOUT_SENSE_NUM = {} mV",
                            app.telemetry.vout_mv,
                            ina_vout,
                            pct,
                            corrected
                        );
                    }
                }
            }

            // embassy's I2C driver waits for events in a blocking spin loop, so a
            // bus that does not answer blocks the whole executor (including
            // `ui_task`) for the per-transaction timeout. A healthy poll takes a
            // few ms; a timing-out one costs ~2 x the I2C timeout. Back off hard
            // when that happens so a missing PD controller cannot starve the UI.
            let pd_took = Instant::now().duration_since(pd_started);
            let pd_bus_up = app.pd_bus_alive();
            pd_period = if !pd_bus_up {
                // No PD bus (XT90/DC input): poll rarely, purely so a USB-C
                // source attached later is noticed. Keeps the log and the bus
                // quiet on a perfectly healthy XT90 session.
                Duration::from_secs(10)
            } else if pd_took > Duration::from_millis(30) {
                Duration::from_secs(2)
            } else {
                Duration::from_millis(board::INA228_POLL_MS)
            };
        }

        {
            let screen = app_state.lock().await.ui.screen;
            if rev3 && screen == MenuScreen::EepromFlash {
                let mut i2c = ui_bus.lock().await;
                eeprom_workflow.update(&mut i2c).await;
                let mut app = app_state.lock().await;
                sync_eeprom_ui(&mut app, &eeprom_workflow);
            }
        }

        Timer::after(Duration::from_micros(100)).await;
    }
}
