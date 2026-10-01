#!/usr/bin/env python3
"""Verify wheel tags describe the native CLI and library in the archive."""
import email
import platform
import re
import struct
import sys
import zipfile
from pathlib import Path

from packaging.tags import mac_platforms
from packaging.utils import parse_wheel_filename

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'bindings' / 'python'))
from wheel_platform import macho_platform


def verify(wheel: Path, sdk: Path) -> None:
    _, _, _, filename_tags = parse_wheel_filename(wheel.name)
    with zipfile.ZipFile(wheel) as archive:
        metadata_names = [n for n in archive.namelist() if n.endswith('.dist-info/WHEEL')]
        assert len(metadata_names) == 1, 'Expected one WHEEL metadata record'
        metadata = email.message_from_bytes(archive.read(metadata_names[0]))
        assert set(metadata.get_all('Tag', [])) == {str(t) for t in filename_tags}, 'Filename and WHEEL tags differ'
        tags = {t.platform for t in filename_tags}
        assert len(tags) == 1, 'Expected one native platform tag'
        tag = tags.pop()
        names = ['plasmite.exe', 'plasmite.dll'] if tag.startswith('win') else ['plasmite', 'libplasmite.dylib' if tag.startswith('macosx_') else 'libplasmite.so']
        binaries = []
        for name in names:
            suffix = 'plasmite/_native/' + name
            members = [n for n in archive.namelist() if n == suffix or n.endswith('/' + suffix)]
            assert len(members) == 1, f'Expected one bundled {name}'
            data = archive.read(members[0])
            source = sdk / ('bin' if name.startswith('plasmite') and not name.endswith('.dll') else 'lib') / name
            assert data == source.read_bytes(), f'{name} differs from selected SDK input'
            binaries.append(data)
        if tag.startswith('macosx_'):
            match = re.fullmatch(r'macosx_(\d+)_(\d+)_(x86_64|arm64)', tag)
            assert match, 'A thin native bundle must have an architecture-specific macOS tag'
            minimum = (int(match[1]), int(match[2]), 0)
            arch = match[3]
            for name, data in zip(names, binaries):
                actual_arch, actual_minimum = macho_platform(data)
                assert actual_arch == arch, f'{name} architecture differs from wheel tag'
                assert minimum >= actual_minimum, f'{name} requires macOS {actual_minimum}, above wheel minimum {minimum}'
            target_tags = set(mac_platforms(version=minimum[:2], arch=arch))
            opposite = 'arm64' if arch == 'x86_64' else 'x86_64'
            opposite_tags = set(mac_platforms(version=minimum[:2], arch=opposite))
            assert tag in target_tags and tag not in opposite_tags, 'Wheel selection accepts the wrong architecture'
            previous = (minimum[0] - 1, 0) if minimum[0] > 11 else (10, 15) if minimum[0] == 11 else (10, minimum[1] - 1)
            assert tag not in set(mac_platforms(version=previous, arch=arch)), 'Wheel selection accepts an OS below its deployment floor'
            assert arch == platform.machine(), 'Official wheel smoke must run on its native architecture'
        elif tag.startswith('win'):
            expected = {'win_amd64': 0x8664, 'win_arm64': 0xaa64}[tag]
            for data in binaries:
                assert data[:2] == b'MZ', 'Expected PE native payload'
                offset = struct.unpack_from('<I', data, 0x3c)[0]
                assert data[offset:offset+4] == b'PE\0\0' and struct.unpack_from('<H', data, offset+4)[0] == expected, 'PE architecture differs from wheel tag'
        elif tag.startswith('linux_'):
            expected = {'linux_x86_64': (2, 62), 'linux_aarch64': (2, 183), 'linux_armv7l': (1, 40)}[tag]
            for data in binaries:
                assert data[:4] == b'\x7fELF' and data[5] == 1, 'Expected little-endian ELF native payload'
                assert (data[4], struct.unpack_from('<H', data, 18)[0]) == expected, 'ELF architecture differs from wheel tag'
        else:
            raise AssertionError('Unsupported wheel platform: ' + tag)
    print('Wheel platform, deployment floor, supported tags and exact SDK bytes PASS: ' + wheel.name)


if __name__ == '__main__':
    verify(Path(sys.argv[1]), Path(sys.argv[2]))
