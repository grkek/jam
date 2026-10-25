/* Memory layout for QEMU `virt` (RAM begins at 0x4000_0000). Skeleton. */

ENTRY(_start)

KERNEL_LOAD_ADDRESS = 0x40080000;
PAGE_SIZE           = 4096;
CORE_COUNT          = 4;
BOOT_STACK_SIZE     = 16384;

SECTIONS
{
    . = KERNEL_LOAD_ADDRESS;

    .text : {
        KEEP(*(.text.boot))
        *(.text .text.*)
    }

    .rodata : ALIGN(PAGE_SIZE) {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(PAGE_SIZE) {
        *(.data .data.*)
    }

    .bss : ALIGN(PAGE_SIZE) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        __bss_end = .;
    }

    .boot_stacks (NOLOAD) : ALIGN(PAGE_SIZE) {
        __boot_stacks_bottom = .;
        . += CORE_COUNT * BOOT_STACK_SIZE;
        __boot_stacks_top = .;
    }

    /DISCARD/ : { *(.comment) *(.eh_frame*) }
}
