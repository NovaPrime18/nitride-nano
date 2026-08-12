//! USB-PD policy: reacts to the TPS26750 interrupt line, (re)negotiates the
//! contract selected in the UI, and mirrors the active contract into the
//! shared supply caps.

use embassy_stm32::exti::ExtiInput;
use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::Async;
use embassy_time::{Duration, Timer};

use crate::board;
use crate::drivers::tps26750::{
    Tps26750, TPS_INT_NEW_CONTRACT_AS_SINK, TPS_INT_PLUG_INSERT_REMOVAL,
};
use crate::state::{AppState, PdState};

/// Stateful wrapper around the TPS26750 driver, polled from the main loop.
pub struct PdManager {
    negotiate_pending: bool,
}

impl PdManager {
    pub fn new() -> Self {
        Self {
            negotiate_pending: false,
        }
    }

    /// One polling step.
    ///
    /// 1. If the TPS26750 IRQ line is asserted, drain the interrupt bitmap and
    ///    arm renegotiation on plug events.
    /// 2. If renegotiation is pending, fetch source caps and request the PDO at
    ///    `app.ui.pd_profile_index`.
    /// 3. Mirror the active contract into the supply's input current/power caps.
    pub async fn poll(
        &mut self,
        tps: &mut Tps26750,
        i2c: &mut I2c<'_, Async, Master>,
        app: &mut AppState,
        irq: &mut ExtiInput<'static>,
    ) {
        let irq_active = irq.is_low();
        if irq_active {
            let mut events = [0u8; 11];
            if tps.read_interrupts(i2c, &mut events).await {
                if Tps26750::is_interrupt_set(&events, TPS_INT_PLUG_INSERT_REMOVAL) {
                    let _ = irq_active;
                    app.pd = PdState::Negotiating;
                    self.negotiate_pending = true;
                }
                if Tps26750::is_interrupt_set(&events, TPS_INT_NEW_CONTRACT_AS_SINK) {
                    app.pd = PdState::ContractActive;
                }
            }
        }

        if self.negotiate_pending {
            app.pd_cap_count = tps.get_source_capabilities(i2c, &mut app.pd_caps).await;
            if app.pd_cap_count > 0 {
                let idx = app.ui.pd_profile_index.min(app.pd_cap_count - 1);
                let cap = app.pd_caps[idx as usize];
                let ok = if cap.is_pps {
                    tps.request_pps_profile(
                        i2c,
                        cap.min_voltage_mv.max(5000),
                        cap.max_current_ma.min(board::IOUT_MAX_MA),
                        cap.min_voltage_mv,
                        cap.voltage_mv,
                    )
                    .await
                } else if cap.is_avs {
                    tps.request_avs_profile(
                        i2c,
                        cap.min_voltage_mv.max(9000),
                        cap.max_current_ma.min(board::IOUT_MAX_MA),
                        cap.min_voltage_mv,
                        cap.voltage_mv,
                    )
                    .await
                } else {
                    tps.request_fixed_profile(
                        i2c,
                        cap.voltage_mv,
                        cap.max_current_ma.min(board::IOUT_MAX_MA),
                    )
                    .await
                };
                if ok {
                    let _ = tps.trigger_renegotiation(i2c).await;
                }
            }
            self.negotiate_pending = false;
        }

        if let Some((v_mv, i_ma)) = tps.get_active_contract(i2c).await {
            app.supply.input_current_cap_ma = i_ma;
            app.supply.input_power_cap_mw = v_mv.saturating_mul(i_ma) / 1000;
            app.telemetry.vin_mv = v_mv;
            if app.pd == PdState::Negotiating {
                app.pd = PdState::ContractActive;
            }
        } else if app.pd == PdState::ContractActive {
            // cable removed
            app.pd = PdState::NoCable;
        }

        let _ = Timer::after(Duration::from_millis(1)).await;
    }
}

impl Default for PdManager {
    fn default() -> Self {
        Self::new()
    }
}
