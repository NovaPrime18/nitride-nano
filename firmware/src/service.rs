//! Service mode: hand the MCU over to the ST ROM bootloader so the board can be
//! reflashed over UART through the on-board FT234XD (USART3, PC10/PC11).
//!
//! [`service_uart_task`] only *signals*; the main loop owns the handoff itself,
//! because it must park the converter before the MCU stops regulating. See
//! [`crate::hal::bootloader`] for the jump and the `.uninit` request handshake.

use core::sync::atomic::{AtomicBool, Ordering};

use embassy_stm32::mode::Async;
use embassy_stm32::usart::Uart;
use embassy_time::{with_timeout, Duration, Timer};

/// Raised by the UART watcher; consumed once by the main loop.
pub static SERVICE_REQUEST: AtomicBool = AtomicBool::new(false);

/// AN3155 bootloader sync byte — what the host tool sends when it connects.
const SYNC: u8 = 0x7F;

/// Watch USART3 for the ROM bootloader's sync byte and raise [`SERVICE_REQUEST`].
///
/// `STM32CubeProgrammer -c` and `stm32flash` both open the port by sending
/// `0x7F`, so merely connecting hands the device over. The host then retries
/// once while the board resets and re-enters the ROM loader.
///
/// The parity mismatch is deliberate and harmless: `0x7F` has an odd number of
/// ones, so the host's *even* parity bit is `1`, which an 8N1 receiver samples
/// as a valid stop bit. This is exactly why `0x7F` is the sync byte.
#[embassy_executor::task]
pub async fn service_uart_task(mut uart: Uart<'static, Async>) {
    // Swallow the line's power-up glitch for a moment so a stray byte cannot
    // reset the board before the user has asked for anything.
    let mut sink = [0u8; 32];
    let _ = with_timeout(Duration::from_millis(200), async {
        loop {
            if uart.read(&mut sink).await.is_err() {
                break;
            }
        }
    })
    .await;

    defmt::info!("service mode: watching USART3 for 0x7F");
    let mut byte = [0u8; 1];
    loop {
        match uart.read(&mut byte).await {
            Ok(()) if byte[0] == SYNC => {
                defmt::info!("service mode: sync byte received");
                SERVICE_REQUEST.store(true, Ordering::SeqCst);
            }
            Ok(()) => {}
            Err(_) => Timer::after(Duration::from_millis(10)).await,
        }
    }
}
