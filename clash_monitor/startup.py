"""Opt-in Windows login startup for the current user."""

from pathlib import Path
import subprocess
import sys
import winreg


RUN_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"
VALUE_NAME = "ClashTrafficMonitor"


def startup_command(data_dir: Path):
    executable = Path(sys.executable).resolve()
    if getattr(sys, "frozen", False):
        args = [str(executable), "--start-hidden"]
    else:
        pythonw = executable.with_name("pythonw.exe")
        # Login startup does not inherit the repository working directory.
        project = str(Path(__file__).resolve().parent.parent)
        code = f"import sys; sys.path.insert(0, {project!r}); from clash_monitor.__main__ import main; sys.exit(main())"
        args = [str(pythonw), "-c", code, "--start-hidden"]
    args.extend(["--data-dir", str(data_dir)])
    return subprocess.list2cmdline(args)


class LoginStartup:
    def __init__(self, data_dir: Path):
        self.data_dir = data_dir.expanduser().resolve()

    def enabled(self):
        try:
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY) as key:
                winreg.QueryValueEx(key, VALUE_NAME)
                return True
        except FileNotFoundError:
            return False

    def set_enabled(self, enabled):
        if enabled:
            with winreg.CreateKeyEx(winreg.HKEY_CURRENT_USER, RUN_KEY, 0, winreg.KEY_SET_VALUE) as key:
                winreg.SetValueEx(key, VALUE_NAME, 0, winreg.REG_SZ, startup_command(self.data_dir))
        else:
            try:
                with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY, 0, winreg.KEY_SET_VALUE) as key:
                    winreg.DeleteValue(key, VALUE_NAME)
            except FileNotFoundError:
                pass
