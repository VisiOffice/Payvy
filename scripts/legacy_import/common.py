"""Small, versioned transfer bundles; no Django or third-party dependencies."""
import hashlib
import json
import os
from decimal import Decimal, InvalidOperation, ROUND_HALF_UP
from pathlib import Path

MAX_BYTES = 25 * 1024 * 1024
MAX_BATCH = 50


def file_kind(data):
    if not data or len(data) > MAX_BYTES:
        raise ValueError('empty file or file exceeds 25 MiB')
    if data.startswith(b'%PDF-'):
        return 'pdf'
    if data.startswith(b'\x89PNG\r\n\x1a\n'):
        return 'png'
    if data.startswith(b'\xff\xd8\xff'):
        return 'jpg'
    if data[:6] in (b'GIF87a', b'GIF89a'):
        return 'gif'
    if data.startswith(b'RIFF') and data[8:12] == b'WEBP':
        return 'webp'
    if data[:4] in (b'II*\x00', b'MM\x00*'):
        return 'tif'
    raise ValueError('unsupported file type')


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def cents(value):
    """Legacy Bill amounts are dollar floats; InboxItemPage amounts already cents."""
    if value is None:
        return None
    try:
        number = Decimal(str(value))
        if not number.is_finite():
            raise ValueError('non-finite legacy amount')
        return int((number * 100).quantize(Decimal('1'), rounding=ROUND_HALF_UP))
    except InvalidOperation as exc:
        raise ValueError('invalid legacy amount') from exc


def write_json(path, value):
    """Atomic replacement, private permissions, durable before continuing uploads."""
    path = Path(path)
    temporary = path.with_name(path.name + '.tmp')
    # O_EXCL also refuses a leftover file/symlink rather than following it.
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(fd, 'w') as stream:
            json.dump(value, stream, indent=2, ensure_ascii=False, allow_nan=False)
            stream.write('\n')
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        temporary.unlink(missing_ok=True)


def bundle_file(root, descriptor):
    """Validate bytes and provenance before any destination writes."""
    if (not isinstance(descriptor, dict) or not isinstance(descriptor.get('path'), str)
            or not isinstance(descriptor.get('sha256'), str)
            or not isinstance(descriptor.get('kind'), str)
            or type(descriptor.get('size_bytes')) is not int):
        raise ValueError('invalid file descriptor')
    relative = Path(descriptor['path'])
    root = Path(root).resolve()
    path = (root / relative).resolve()
    if relative.is_absolute() or not path.is_relative_to(root) or '..' in relative.parts:
        raise ValueError('file path escapes bundle')
    with path.open('rb') as stream:
        data = stream.read(MAX_BYTES + 1)
    if file_kind(data) != descriptor['kind']:
        raise ValueError('file type differs from manifest')
    if len(data) != descriptor['size_bytes'] or sha256(data) != descriptor['sha256']:
        raise ValueError('file size or hash differs from manifest')
    return data
