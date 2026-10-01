"""Derive a macOS wheel platform from the native files that it ships."""
from __future__ import annotations

import platform
import re
import struct
from pathlib import Path


def macho_platform(data: bytes) -> tuple[str, tuple[int, int, int]]:
    """Read a thin 64-bit Mach-O CPU and its minimum macOS deployment version."""
    endian = {b'\xcf\xfa\xed\xfe': '<', b'\xfe\xed\xfa\xcf': '>'}.get(data[:4])
    if endian is None or len(data) < 32:
        raise ValueError('Expected a thin 64-bit macOS native binary')
    cpu, _, _, count, commands_size = struct.unpack_from(endian + 'IIIII', data, 4)
    arch = {0x01000007: 'x86_64', 0x0100000c: 'arm64'}.get(cpu)
    if arch is None:
        raise ValueError(f'Unsupported macOS native CPU: {cpu:#x}')
    end = 32 + commands_size
    if end > len(data):
        raise ValueError('Truncated Mach-O load commands')
    offset = 32
    versions = []
    for _ in range(count):
        if offset + 8 > end:
            raise ValueError('Truncated Mach-O load command')
        command, size = struct.unpack_from(endian + 'II', data, offset)
        if size < 8 or offset + size > end:
            raise ValueError('Invalid Mach-O load command size')
        if command == 0x32:  # LC_BUILD_VERSION
            if size < 24:
                raise ValueError('Truncated LC_BUILD_VERSION')
            target, version = struct.unpack_from(endian + 'II', data, offset + 8)
            if target != 1:
                raise ValueError('Native bundle must target macOS')
            versions.append(version)
        elif command == 0x24:  # LC_VERSION_MIN_MACOSX
            if size < 16:
                raise ValueError('Truncated LC_VERSION_MIN_MACOSX')
            versions.append(struct.unpack_from(endian + 'I', data, offset + 8)[0])
        offset += size
    if not versions:
        raise ValueError('Native binary has no macOS deployment minimum')
    version = max(versions)
    return arch, (version >> 16, (version >> 8) & 255, version & 255)


def macos_platform_tag(native_dir: Path, original_tag: str) -> str:
    match = re.fullmatch(r'macosx_(\d+)_(\d+)_.+', original_tag)
    if match is None:
        raise ValueError(f'Invalid macOS wheel platform: {original_tag}')
    minimum = (int(match[1]), int(match[2]), 0)
    binaries = [native_dir / name for name in ('libplasmite.dylib', 'plasmite')]
    native = [macho_platform(p.read_bytes()) for p in binaries if p.is_file()]
    architectures = {arch for arch, _ in native}
    if len(architectures) > 1:
        raise ValueError('Bundled macOS CLI and library have different architectures')
    arch = next(iter(architectures), platform.machine())
    if arch not in {'x86_64', 'arm64'}:
        raise ValueError(f'Unsupported macOS wheel architecture: {arch}')
    minimum = max([minimum, *(version for _, version in native)])
    major, minor, patch = minimum
    # Wheel tags cannot encode patch versions; modern macOS tags use major_0.
    if major >= 11:
        major += bool(minor or patch)
        minor = 0
    elif patch:
        minor += 1
    return f'macosx_{major}_{minor}_{arch}'
