//! Boot-time BCM2711 hardware layout for the Raspberry Pi 4 Model B.
//!
//! Requires `arm_peri_high=0` and `dtoverlay=disable-bt`. UART0 on GPIO14 and GPIO15 is
//! configured by [`initialise_console_hardware`]. This first port intentionally initializes
//! one core; native Raspberry Pi secondary-core release is a separate platform service.
//!
//! Sources: BCM2711 ARM Peripherals, and Raspberry Pi firmware documentation.

use foundation::hardware_description::PhysicalRegion;

use crate::memory::address_translation::physical_to_virtual;

use super::EarlyDeviceMemoryRegion;

/// ARM physical base of PL011 UART0, provided by the BCM2711 peripheral bus.
pub const PL011_UART_PHYSICAL_BASE_ADDRESS: usize = 0xfe20_1000;
/// ARM physical base of the GIC-400 distributor in low-peripheral mode.
pub const GIC_DISTRIBUTOR_PHYSICAL_BASE_ADDRESS: usize = 0xff84_1000;
/// ARM physical base of the GIC-400 CPU interface in low-peripheral mode.
pub const GIC_CPU_INTERFACE_PHYSICAL_BASE_ADDRESS: usize = 0xff84_2000;

/// Initial normal-memory mapping, limited to the first 1 GiB of physical RAM.
///
/// The allocator must not manage addresses outside this interval until JAM
/// supports dynamic boot mappings for larger or discontiguous RAM banks.
pub const BOOTSTRAP_MAPPED_RAM: PhysicalRegion = PhysicalRegion {
    start_address: 0x0000_0000,
    byte_count: 0x4000_0000,
};

/// Low-peripheral-mode MMIO window containing UART0 and both GIC interfaces.
///
/// The 32 MiB interval is aligned for JAM's 2 MiB device-block mapper.
pub const EARLY_DEVICE_REGIONS: [EarlyDeviceMemoryRegion; 1] = [EarlyDeviceMemoryRegion {
    start_address: 0xfe00_0000,
    end_address: 0x1_0000_0000,
}];

/// The Pi firmware must pass the flattened device tree address in register x0.
///
/// Blindly probing a hard-coded address on real hardware can fault or read
/// firmware-owned memory. Unlike QEMU, no fallback address is assumed.
pub const DEVICE_TREE_FALLBACK_ADDRESS: Option<usize> = None;

/// Prevents native secondary-core startup until BCM2711 release is implemented.
pub const SECONDARY_CORE_STARTUP_SUPPORTED: bool = false;

/// ARM physical base of the GPIO block.
const GPIO_PHYSICAL_BASE_ADDRESS: usize = 0xfe20_0000;
/// Offset of `GPFSEL1`, which selects the function of GPIO10 to GPIO19.
const GPIO_FUNCTION_SELECT_1_OFFSET: usize = 0x04;
/// Offset of `GPIO_PUP_PDN_CNTRL_REG0`, the BCM2711 pull control for GPIO0 to GPIO15.
const GPIO_PULL_CONTROL_0_OFFSET: usize = 0xe4;
/// Mask for one 3-bit pin function field in `GPFSEL1`.
const GPIO_FUNCTION_FIELD_MASK: u32 = 0b111;
/// Function field value that selects alternate function 0 (PL011 TXD0 and RXD0).
const GPIO_FUNCTION_ALTERNATE_0: u32 = 0b100;
/// Bit position of GPIO14 in `GPFSEL1`.
const GPIO14_FUNCTION_SHIFT: u32 = 12;
/// Bit position of GPIO15 in `GPFSEL1`.
const GPIO15_FUNCTION_SHIFT: u32 = 15;
/// Mask for one 2-bit pull field in `GPIO_PUP_PDN_CNTRL_REG0`. Zero means no pull resistor.
const GPIO_PULL_FIELD_MASK: u32 = 0b11;
/// Bit position of GPIO14 in `GPIO_PUP_PDN_CNTRL_REG0`.
const GPIO14_PULL_SHIFT: u32 = 28;
/// Bit position of GPIO15 in `GPIO_PUP_PDN_CNTRL_REG0`.
const GPIO15_PULL_SHIFT: u32 = 30;

/// Offset of the PL011 flag register.
const PL011_FLAG_OFFSET: usize = 0x18;
/// Offset of the PL011 integer baud rate divisor.
const PL011_INTEGER_BAUD_OFFSET: usize = 0x24;
/// Offset of the PL011 fractional baud rate divisor.
const PL011_FRACTIONAL_BAUD_OFFSET: usize = 0x28;
/// Offset of the PL011 line control register.
const PL011_LINE_CONTROL_OFFSET: usize = 0x2c;
/// Offset of the PL011 control register.
const PL011_CONTROL_OFFSET: usize = 0x30;
/// Offset of the PL011 interrupt clear register.
const PL011_INTERRUPT_CLEAR_OFFSET: usize = 0x44;
/// Flag register bit that is set while the UART is transmitting.
const PL011_FLAG_BUSY: u32 = 1 << 3;
/// Interrupt clear value that clears every PL011 interrupt.
const PL011_INTERRUPT_CLEAR_ALL: u32 = 0x7ff;
/// Integer baud divisor for 115200 baud from the 48 MHz UART clock.
const PL011_INTEGER_BAUD_115200: u32 = 26;
/// Fractional baud divisor for 115200 baud from the 48 MHz UART clock.
const PL011_FRACTIONAL_BAUD_115200: u32 = 3;
/// Line control value for 8 data bits, no parity, one stop bit, FIFOs enabled.
const PL011_LINE_CONTROL_8N1_FIFO: u32 = 0x70;
/// Control value that enables the UART, its transmitter and its receiver.
const PL011_CONTROL_ENABLED: u32 = 0x301;

/// Reads a 32-bit device register.
///
/// # Safety
///
/// `address` must be a mapped, 4-byte-aligned device register.
unsafe fn read_register(address: usize) -> u32 {
    // SAFETY: The caller guarantees that `address` is a valid device register.
    unsafe { core::ptr::read_volatile(address as *const u32) }
}

/// Writes a 32-bit device register.
///
/// # Safety
///
/// `address` must be a mapped, 4-byte-aligned device register.
unsafe fn write_register(address: usize, value: u32) {
    // SAFETY: The caller guarantees that `address` is a valid device register.
    unsafe { core::ptr::write_volatile(address as *mut u32, value) }
}

/// Routes PL011 UART0 to GPIO14 and GPIO15 and configures it for 115200 baud, 8N1.
///
/// Assumes the firmware leaves the UART clock at its 48 MHz default and that
/// `dtoverlay=disable-bt` frees UART0 from the Bluetooth controller. Call this after the
/// upper-half device mapping is live and before the first console output.
pub fn initialise_console_hardware() {
    let gpio = physical_to_virtual(GPIO_PHYSICAL_BASE_ADDRESS);
    let uart = physical_to_virtual(PL011_UART_PHYSICAL_BASE_ADDRESS);

    // SAFETY: Both blocks lie inside the device-mapped early window, and every offset is a
    // 4-byte-aligned register of the BCM2711 GPIO block or the PL011.
    unsafe {
        let function_select = gpio + GPIO_FUNCTION_SELECT_1_OFFSET;
        let function_mask = (GPIO_FUNCTION_FIELD_MASK << GPIO14_FUNCTION_SHIFT)
            | (GPIO_FUNCTION_FIELD_MASK << GPIO15_FUNCTION_SHIFT);
        let function_value = (GPIO_FUNCTION_ALTERNATE_0 << GPIO14_FUNCTION_SHIFT)
            | (GPIO_FUNCTION_ALTERNATE_0 << GPIO15_FUNCTION_SHIFT);
        write_register(
            function_select,
            (read_register(function_select) & !function_mask) | function_value,
        );

        let pull_control = gpio + GPIO_PULL_CONTROL_0_OFFSET;
        let pull_mask = (GPIO_PULL_FIELD_MASK << GPIO14_PULL_SHIFT)
            | (GPIO_PULL_FIELD_MASK << GPIO15_PULL_SHIFT);
        write_register(pull_control, read_register(pull_control) & !pull_mask);

        // Disable the UART and let any transmission in flight finish before reconfiguring.
        write_register(uart + PL011_CONTROL_OFFSET, 0);
        while read_register(uart + PL011_FLAG_OFFSET) & PL011_FLAG_BUSY != 0 {}

        write_register(
            uart + PL011_INTERRUPT_CLEAR_OFFSET,
            PL011_INTERRUPT_CLEAR_ALL,
        );
        write_register(uart + PL011_INTEGER_BAUD_OFFSET, PL011_INTEGER_BAUD_115200);
        write_register(
            uart + PL011_FRACTIONAL_BAUD_OFFSET,
            PL011_FRACTIONAL_BAUD_115200,
        );
        write_register(
            uart + PL011_LINE_CONTROL_OFFSET,
            PL011_LINE_CONTROL_8N1_FIFO,
        );
        write_register(uart + PL011_CONTROL_OFFSET, PL011_CONTROL_ENABLED);
    }
}
