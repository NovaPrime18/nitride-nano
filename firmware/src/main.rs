#![no_std]
#![no_main]

use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_stm32::adc::{Adc, Resolution, SampleTime};
use embassy_stm32::dac::{DacCh1, DacChannel, Value};
use embassy_stm32::exti::ExtiInput;
use embassy_stm32::gpio::{Input, Level, Output, Pull, Speed};
use embassy_stm32::i2c::{Config as I2cConfig, I2c};
use embassy_stm32::pac::vrefbuf::vals::{Hiz, Vrs};
use embassy_stm32::rcc::*;
use embassy_stm32::timer::qei::{Qei, QeiPin};
use embassy_stm32::vrefbuf::VoltageReferenceBuffer;
use embassy_stm32::{bind_interrupts, peripherals, Config};
use embassy_time::{Duration, Instant, Timer};
use panic_probe as _;
// remove: use panic_halt as _;

use nitride_firmware::board;
use nitride_firmware::control::supply::SupplyController;
use nitride_firmware::drivers::ssd1306_ui::Ssd1306Ui;
use nitride_firmware::eeprom_workflow::EepromWorkflow;
//use nitride_firmware::drivers::ssd1306_init::initialize_ssd1306;  // Correct the import
// use nitride_firmware::drivers::ssd1306_init::initialize_ssd1306;  // Comment out the unused import for now
use nitride_firmware::drivers::tps26750::Tps26750;
use nitride_firmware::hal::converter_enable::ConverterEnable;
use nitride_firmware::pd::manager::PdManager;
use nitride_firmware::sense::adc_sense::{AdcSense, TelemetryFilter};
use nitride_firmware::state::{AppState, MenuScreen};
use nitride_firmware::ui::input::InputHandler;
use nitride_firmware::ui::menu::apply_input;

bind_interrupts!(struct Irqs {
    I2C3_EV => embassy_stm32::i2c::EventInterruptHandler<peripherals::I2C3>;
    I2C3_ER => embassy_stm32::i2c::ErrorInterruptHandler<peripherals::I2C3>;
});

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let mut config = Config::default();
    config.rcc.mux.adc12sel = mux::Adcsel::SYS;
    config.rcc.mux.adc345sel = mux::Adcsel::SYS;

    let p = embassy_stm32::init(config);

    let _vref = VoltageReferenceBuffer::new(p.VREFBUF, Vrs::VREF1, Hiz::CONNECTED);

    let mut dac_cv: DacCh1<'_, embassy_stm32::peripherals::DAC1, embassy_stm32::mode::Blocking> =
        DacChannel::new_blocking(p.DAC1, p.PA4);
    let mut dac_cc: DacCh1<'_, embassy_stm32::peripherals::DAC2, embassy_stm32::mode::Blocking> =
        DacChannel::new_blocking(p.DAC2, p.PA6);
    dac_cv.set(Value::Bit12Right(0));
    dac_cc.set(Value::Bit12Right(0));

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

    let qei = Qei::new(p.TIM4, QeiPin::new(p.PB6), QeiPin::new(p.PA12));
    let mut enc_last: u16 = qei.count();
    let mut pd_irq = ExtiInput::new(p.PB13, p.EXTI13, Pull::Up);

    let mut i2c_config = I2cConfig::default();
    i2c_config.frequency = embassy_stm32::time::Hertz::khz(400);

    let mut i2c = I2c::new(
        p.I2C3,
        p.PA8,
        p.PB5,
        Irqs,
        p.DMA1_CH3,
        p.DMA1_CH4,
        i2c_config, // <--- Pass it here
    );
    let mut app = AppState::default();
    let mut supply = SupplyController::new();
    let mut sense = AdcSense::new();
    let mut input = InputHandler::new();
    // No change needed here, but ensure the function is correctly imported
    let mut ui = Ssd1306Ui::new(); // Remove the display argument
    let mut tele_filter = TelemetryFilter::new();
    let mut pd_mgr = PdManager::new();
    let mut tps = Tps26750::new(board::TPS26750_ADDR);
    let mut eeprom_workflow = EepromWorkflow::new();
    defmt::info!("Display init OK");

    // in your draw loop, temporarily:
    defmt::info!("Drawing frame...");
    let result = ui.init(&mut i2c).await;
    match result {
        Ok(_) => defmt::info!("Flush OK"),
        Err(_) => defmt::error!("Flush FAILED - I2C error"),
    }
    if let Err(e) = result {
        panic!("Failed to initialize SSD1306: {:?}", e);
    }
    if tps.init(&mut i2c).await {
        app.pd_cap_count = tps
            .get_source_capabilities(&mut i2c, &mut app.pd_caps)
            .await;
    }

    let mut t_adc = Instant::now();
    let mut t_supply = Instant::now();
    let mut t_input = Instant::now();
    let mut t_ui = Instant::now();
    let mut t_pd = Instant::now();

    loop {
        let now = Instant::now();

        if now.duration_since(t_input) >= Duration::from_millis(board::INPUT_POLL_MS) {
            t_input = now;
            let c = qei.count();
            let delta = (c.wrapping_sub(enc_last)) as i16;
            enc_last = c;
            input.poll(&btn1, &btn2, &btn3, &enc_btn, delta);
            if let Some(ev) = input.last_event {
                let previous_screen = app.ui.screen;
                if app.ui.screen == MenuScreen::EepromFlash {
                    eeprom_workflow.handle_input(ev);
                }
                apply_input(&mut app, ev);
                if previous_screen != app.ui.screen && app.ui.screen == MenuScreen::EepromFlash {
                    defmt::info!("Entered EEPROM flash screen");
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
            app.telemetry = tele_filter.filter(raw);
        }

        if now.duration_since(t_supply) >= Duration::from_millis(board::SUPPLY_TICK_MS) {
            t_supply = now;
            supply.tick(&mut app, &mut dac_cv, &mut dac_cc, &mut conv_en);
        }

        if now.duration_since(t_pd) >= Duration::from_millis(100) {
            t_pd = now;
            pd_mgr.poll(&mut tps, &mut i2c, &mut app, &mut pd_irq).await;
        }

        if app.ui.screen == MenuScreen::EepromFlash {
            eeprom_workflow.update(&mut i2c).await;
        }

        if now.duration_since(t_ui) >= Duration::from_millis(board::UI_REFRESH_MS) {
            t_ui = now;
            if app.ui.screen == MenuScreen::EepromFlash {
                ui.draw_eeprom_screen(
                    &mut i2c,
                    eeprom_workflow.title(),
                    eeprom_workflow.message(),
                    eeprom_workflow.progress_percent(),
                )
                .await
                .ok();
            } else if app.ui.screen == MenuScreen::PdContract {
                ui.draw_pd_contract_screen(&mut i2c, &app).await.ok();
            } else {
                ui.draw_power_screen(&mut i2c, &app).await.ok();
            }
        }

        Timer::after(Duration::from_micros(100)).await;
    }
}
