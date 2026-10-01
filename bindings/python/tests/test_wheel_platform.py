"""Keep macOS wheel selection aligned with both bundled native programs."""
import struct
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from wheel_platform import macos_platform_tag, macho_platform


def binary(arch, minimum, legacy=False):
    cpu = {'x86_64': 0x01000007, 'arm64': 0x0100000c}[arch]
    version = minimum[0] << 16 | minimum[1] << 8 | minimum[2]
    command = struct.pack('<IIII', 0x24, 16, version, 0) if legacy else struct.pack('<IIIIII', 0x32, 24, 1, version, 0, 0)
    return struct.pack('<IIIIIIII', 0xfeedfacf, cpu, 0, 2, 1, len(command), 0, 0) + command


class WheelPlatformTests(unittest.TestCase):
    def tag(self, library, cli, original='macosx_11_0_universal2'):
        with tempfile.TemporaryDirectory() as directory:
            native = Path(directory)
            (native / 'libplasmite.dylib').write_bytes(library)
            (native / 'plasmite').write_bytes(cli)
            return macos_platform_tag(native, original)

    def test_arm_payload_cannot_claim_universal2(self):
        arm = binary('arm64', (11, 0, 0))
        self.assertEqual(self.tag(arm, arm), 'macosx_11_0_arm64')

    def test_intel_payload_keeps_legacy_deployment_floor(self):
        intel = binary('x86_64', (10, 12, 0), legacy=True)
        self.assertEqual(self.tag(intel, intel, 'macosx_10_12_universal2'), 'macosx_10_12_x86_64')

    def test_cli_minimum_can_raise_wheel_floor(self):
        self.assertEqual(self.tag(binary('arm64', (11, 0, 0)), binary('arm64', (14, 0, 0))), 'macosx_14_0_arm64')

    def test_interpreter_minimum_can_raise_wheel_floor(self):
        arm = binary('arm64', (11, 0, 0))
        self.assertEqual(self.tag(arm, arm, 'macosx_12_0_universal2'), 'macosx_12_0_arm64')

    def test_mixed_native_architectures_fail(self):
        with self.assertRaisesRegex(ValueError, 'different architectures'):
            self.tag(binary('arm64', (11, 0, 0)), binary('x86_64', (11, 0, 0)))

    def test_unrepresentable_minimum_rounds_up(self):
        for arch, minimum, expected in [('arm64', (11, 1, 0), 'macosx_12_0_arm64'), ('arm64', (11, 0, 1), 'macosx_12_0_arm64'), ('x86_64', (10, 12, 1), 'macosx_10_13_x86_64')]:
            with self.subTest(minimum=minimum):
                artifact = binary(arch, minimum)
                self.assertEqual(self.tag(artifact, artifact, 'macosx_10_12_universal2'), expected)

    def test_missing_optional_bundle_uses_process_architecture(self):
        with tempfile.TemporaryDirectory() as directory, patch('wheel_platform.platform.machine', return_value='x86_64'):
            self.assertEqual(macos_platform_tag(Path(directory), 'macosx_10_12_universal2'), 'macosx_10_12_x86_64')

    def test_truncated_and_fat_binaries_fail_closed(self):
        for data in [binary('arm64', (11, 0, 0))[:-1], b'\xca\xfe\xba\xbe' + bytes(28)]:
            with self.subTest(data=data[:4]), self.assertRaises(ValueError):
                macho_platform(data)
