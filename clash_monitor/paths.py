"""Shared data locations that remain outside Windows AppData virtualization."""

import os
from pathlib import Path


def default_data_dir() -> Path:
    profile = os.environ.get("USERPROFILE")
    try:
        home = Path(profile) if profile else Path.home()
    except RuntimeError:
        raise OSError("无法确定 Windows 用户目录，请通过 --data-dir 指定数据目录。") from None
    if not home.is_absolute():
        raise OSError("Windows 用户目录不是绝对路径，请通过 --data-dir 指定数据目录。")
    return (home / "Documents" / "ClashTrafficMonitor").resolve()


def resolve_data_dir(directory: Path | None = None) -> Path:
    return directory.expanduser().resolve() if directory is not None else default_data_dir()
