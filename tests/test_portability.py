"""Host-only regression tests for JAM's phase-one platform installer."""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


PACKAGE = Path(__file__).resolve().parents[1]
SPECIFICATION = importlib.util.spec_from_file_location(
    "jam_portability_installer", PACKAGE / "apply_portability.py"
)
assert SPECIFICATION is not None and SPECIFICATION.loader is not None
INSTALLER = importlib.util.module_from_spec(SPECIFICATION)
SPECIFICATION.loader.exec_module(INSTALLER)


class PortabilityInstallerTests(unittest.TestCase):
    """Check platform address preservation and safe source rewriting."""

    def test_kernel_map_preserves_guard_regions(self) -> None:
        """Do not erase existing boot, idle, interrupt or reclamation guards."""
        original = (PACKAGE / "reference/kernel_map_no_emergency.rs").read_text()
        self.assertIn("__boot_stacks_bottom", original)
        self.assertIn("__idle_stacks_bottom", original)
        self.assertIn("__interrupt_stacks_bottom", original)
        self.assertIn("__reclamation_stacks_bottom", original)
        self.assertEqual(INSTALLER.update_kernel_map(original), original)

    def test_kernel_map_refuses_unknown_layout(self) -> None:
        """Never blindly overwrite unfamiliar VM mapping code."""
        with self.assertRaises(ValueError):
            INSTALLER.update_kernel_map("// Unexpected map implementation\n")

    def test_update_main_is_idempotent(self) -> None:
        """Register the platform module at most once."""
        original = "//! Kernel\n\npub mod scheduling;\n"
        first = INSTALLER.update_main(original)
        self.assertEqual(first.count("pub mod platform;"), 1)
        self.assertEqual(INSTALLER.update_main(first), first)

    def test_console_and_gic_reference_platform(self) -> None:
        """Ensure source files do not embed QEMU-specific physical bases."""
        console = (PACKAGE / INSTALLER.EARLY_CONSOLE).read_text()
        controller = (PACKAGE / INSTALLER.INTERRUPT_CONTROLLER).read_text()
        self.assertIn("PL011_UART_PHYSICAL_BASE_ADDRESS", console)
        self.assertIn("GIC_DISTRIBUTOR_PHYSICAL_BASE_ADDRESS", controller)
        self.assertIn("GIC_CPU_INTERFACE_PHYSICAL_BASE_ADDRESS", controller)
        self.assertNotIn("KERNEL_VIRTUAL_OFFSET", console)
        self.assertNotIn("KERNEL_VIRTUAL_OFFSET", controller)


if __name__ == "__main__":
    unittest.main()
