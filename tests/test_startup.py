from pathlib import Path
import unittest
from unittest.mock import patch

from clash_monitor import startup


class Registry:
    HKEY_CURRENT_USER = "current-user"
    KEY_SET_VALUE = 2
    REG_SZ = 1

    def __init__(self):
        self.values = {"OtherApp": "keep"}
        self.writes = []

    def OpenKey(self, root, path, *_):
        self.assert_location(root, path)
        return self

    CreateKeyEx = OpenKey

    def __enter__(self):
        return self

    def __exit__(self, *_):
        pass

    def assert_location(self, root, path):
        assert root == self.HKEY_CURRENT_USER and path == startup.RUN_KEY

    def QueryValueEx(self, key, name):
        if name not in self.values:
            raise FileNotFoundError(name)
        return self.values[name], self.REG_SZ

    def SetValueEx(self, key, name, reserved, kind, value):
        self.writes.append(name)
        self.values[name] = value

    def DeleteValue(self, key, name):
        if name not in self.values:
            raise FileNotFoundError(name)
        self.writes.append(name)
        del self.values[name]


class LoginStartupTests(unittest.TestCase):
    def test_opt_in_and_repeated_toggle_touch_only_owned_value(self):
        registry = Registry()
        with patch.object(startup, "winreg", registry):
            setting = startup.LoginStartup(Path(r"C:\Users\Example\Documents\ClashTrafficMonitor"))
            self.assertFalse(setting.enabled())
            self.assertEqual(registry.writes, [])
            setting.set_enabled(False)
            for _ in range(2):
                setting.set_enabled(True)
                self.assertTrue(setting.enabled())
                self.assertIn("--start-hidden", registry.values[startup.VALUE_NAME])
                self.assertIn("--data-dir", registry.values[startup.VALUE_NAME])
            for _ in range(2):
                setting.set_enabled(False)
                self.assertFalse(setting.enabled())
        self.assertEqual(registry.values, {"OtherApp": "keep"})
        self.assertEqual(set(registry.writes), {startup.VALUE_NAME})

    def test_packaged_path_with_spaces(self):
        with patch.object(startup.sys, "frozen", True, create=True), patch.object(
            startup.sys, "executable", r"C:\Program Files\ClashTrafficMonitor.exe"
        ):
            self.assertEqual(
                startup.startup_command(Path(r"C:\History Shared\Clash Monitor")),
                r'"C:\Program Files\ClashTrafficMonitor.exe" --start-hidden --data-dir "C:\History Shared\Clash Monitor"',
            )

    def test_source_login_carries_absolute_data_directory(self):
        data_dir = Path(r"C:\History Shared\Clash Monitor")
        with patch.object(startup.sys, "frozen", False, create=True), patch.object(
            startup.sys, "executable", r"C:\Python313\python.exe"
        ):
            command = startup.startup_command(data_dir)
        self.assertTrue(command.startswith(r"C:\Python313\pythonw.exe -c "))
        self.assertIn("sys.path.insert", command)
        self.assertTrue(command.endswith(r'--start-hidden --data-dir "C:\History Shared\Clash Monitor"'))
