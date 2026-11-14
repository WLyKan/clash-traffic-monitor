"""Development-only guarded Win32 tray acceptance for an explicit D: instance.

inspect is the default and sends no window messages. open and menu-command
must be explicitly selected by the operator. Every action revalidates PID
creation time, executable hash, and the exact physical --data-dir argument.
No process is ever killed. This script is not part of the packaged application.
"""
import argparse
import ctypes as C
from ctypes import wintypes as W
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import sys
import time

import psutil

ROOT = Path("D:/DevCache/ClashTrafficMonitorTauri")
COMMANDS = ("打开面板", "查看最近流量提醒", "退出")
WM_USER_TRAYICON = 6002
WM_RBUTTONUP = 517
WM_COMMAND = 0x0111
WM_CANCELMODE = 0x001F
MN_GETHMENU = 481
STILL_ACTIVE = 259


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


class ActionFailure(RuntimeError):
    def __init__(self, error, report):
        super().__init__(f"{type(error).__name__}: {error}")
        self.report = report


def post(native, hwnd, message, wparam, lparam, report, name):
    posted = bool(native.user.PostMessageW(hwnd, message, wparam, lparam))
    report["actions"].append({"api": "PostMessageW", "message": name, "message_id": message,
        "hwnd": hwnd, "wParam": wparam, "lParam": lparam, "posted": posted,
        "posted_at_utc": datetime.now(timezone.utc).isoformat()})
    require(posted, f"posting verified {name} failed")


def timestamp(value):
    return datetime.fromtimestamp(value, timezone.utc).isoformat(timespec="microseconds")


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def data_argument(arguments):
    found = []
    for index, value in enumerate(arguments):
        if value == "--data-dir":
            require(index+1 < len(arguments), "process --data-dir has no value")
            found.append(arguments[index+1])
        elif value.startswith("--data-dir="):
            found.append(value[len("--data-dir="):])
    require(len(found) == 1 and found[0], "process must have exactly one explicit --data-dir")
    require(Path(found[0]).is_absolute(), "process --data-dir must be absolute")
    return Path(found[0]).resolve(strict=True)


class GuiThreadInfo(C.Structure):
    _fields_ = [("cbSize", W.DWORD), ("flags", W.DWORD),
                ("hwndActive", W.HWND), ("hwndFocus", W.HWND),
                ("hwndCapture", W.HWND), ("hwndMenuOwner", W.HWND),
                ("hwndMoveSize", W.HWND), ("hwndCaret", W.HWND), ("rcCaret", W.RECT)]


class MenuItemInfo(C.Structure):
    _fields_ = [("cbSize", W.UINT), ("fMask", W.UINT), ("fType", W.UINT),
                ("fState", W.UINT), ("wID", W.UINT), ("hSubMenu", W.HMENU),
                ("hbmpChecked", W.HBITMAP), ("hbmpUnchecked", W.HBITMAP),
                ("dwItemData", C.c_size_t), ("dwTypeData", W.LPWSTR),
                ("cch", W.UINT), ("hbmpItem", W.HBITMAP)]


class Native:
    def __init__(self):
        self.user = C.WinDLL("user32", use_last_error=True)
        self.kernel = C.WinDLL("kernel32", use_last_error=True)
        self.callback_type = C.WINFUNCTYPE(W.BOOL, W.HWND, W.LPARAM)
        self.user.EnumWindows.argtypes = [self.callback_type, W.LPARAM]
        self.user.EnumWindows.restype = W.BOOL
        self.user.FindWindowExW.argtypes = [W.HWND, W.HWND, W.LPCWSTR, W.LPCWSTR]
        self.user.FindWindowExW.restype = W.HWND
        self.user.GetWindowThreadProcessId.argtypes = [W.HWND, C.POINTER(W.DWORD)]
        self.user.GetWindowThreadProcessId.restype = W.DWORD
        self.user.GetClassNameW.argtypes = [W.HWND, W.LPWSTR, C.c_int]
        self.user.GetClassNameW.restype = C.c_int
        self.user.GetGUIThreadInfo.argtypes = [W.DWORD, C.POINTER(GuiThreadInfo)]
        self.user.GetGUIThreadInfo.restype = W.BOOL
        self.user.PostMessageW.argtypes = [W.HWND, W.UINT, W.WPARAM, W.LPARAM]
        self.user.PostMessageW.restype = W.BOOL
        self.user.SendMessageTimeoutW.argtypes = [W.HWND, W.UINT, W.WPARAM, W.LPARAM,
                                                W.UINT, W.UINT, C.POINTER(C.c_size_t)]
        self.user.SendMessageTimeoutW.restype = W.LPARAM
        self.user.GetMenuItemCount.argtypes = [W.HMENU]
        self.user.GetMenuItemCount.restype = C.c_int
        self.user.GetMenuItemInfoW.argtypes = [W.HMENU, W.UINT, W.BOOL, C.POINTER(MenuItemInfo)]
        self.user.GetMenuItemInfoW.restype = W.BOOL
        self.kernel.OpenProcess.argtypes = [W.DWORD, W.BOOL, W.DWORD]
        self.kernel.OpenProcess.restype = W.HANDLE
        self.kernel.WaitForSingleObject.argtypes = [W.HANDLE, W.DWORD]
        self.kernel.WaitForSingleObject.restype = W.DWORD
        self.kernel.GetExitCodeProcess.argtypes = [W.HANDLE, C.POINTER(W.DWORD)]
        self.kernel.GetExitCodeProcess.restype = W.BOOL
        self.kernel.CloseHandle.argtypes = [W.HANDLE]
        self.kernel.CloseHandle.restype = W.BOOL

    def class_name(self, hwnd):
        buffer = C.create_unicode_buffer(256)
        return buffer.value if self.user.GetClassNameW(hwnd, buffer, len(buffer)) else ""

    def window_identity(self, hwnd):
        pid = W.DWORD()
        thread = self.user.GetWindowThreadProcessId(hwnd, C.byref(pid))
        return pid.value, thread

    def windows(self, class_name, pid, thread=None):
        result = set()
        def visit(hwnd, _):
            owner_pid, owner_thread = self.window_identity(hwnd)
            if owner_pid == pid and (thread is None or thread == owner_thread) and self.class_name(hwnd) == class_name:
                result.add(int(hwnd))
            return True
        callback = self.callback_type(visit)
        require(self.user.EnumWindows(callback, 0), "EnumWindows failed")
        if class_name == "tray_icon_app":
            # Include message-only tray windows without relying on visibility.
            previous = None
            while True:
                hwnd = self.user.FindWindowExW(W.HWND(-3), previous, class_name, None)
                if not hwnd:
                    break
                visit(hwnd, 0)
                previous = hwnd
        return sorted(result)

    def tray(self, pid):
        matches = self.windows("tray_icon_app", pid)
        require(len(matches) == 1, f"expected one target-PID tray_icon_app, found {len(matches)}")
        hwnd = matches[0]
        owner_pid, thread = self.window_identity(hwnd)
        require(owner_pid == pid and thread, "tray identity changed")
        return hwnd, thread

    def gui(self, thread):
        info = GuiThreadInfo(cbSize=C.sizeof(GuiThreadInfo))
        require(self.user.GetGUIThreadInfo(thread, C.byref(info)), "GetGUIThreadInfo failed")
        return info

    def popup(self, pid, tray, thread):
        info = self.gui(thread)
        require(int(info.hwndMenuOwner or 0) == tray, "active native menu owner is not the verified tray HWND")
        matches = self.windows("#32768", pid, thread)
        require(len(matches) == 1, f"expected one target-PID/thread native popup, found {len(matches)}")
        return matches[0]

    def menu(self, pid, tray, thread):
        popup = self.popup(pid, tray, thread)
        menu = C.c_size_t()
        require(self.user.SendMessageTimeoutW(popup, MN_GETHMENU, 0, 0, 0x0002, 1000, C.byref(menu)),
                "MN_GETHMENU did not return from verified popup")
        require(menu.value, "verified popup has no HMENU")
        items = self.menu_items(menu.value)
        require(self.popup(pid, tray, thread) == popup, "popup changed while reading menu")
        return {"popup_hwnd": popup, "menu_handle": menu.value, "owner_hwnd": tray, "thread_id": thread, "items": items}

    def menu_items(self, menu):
        count = self.user.GetMenuItemCount(menu)
        require(count == len(COMMANDS), f"unexpected native menu item count: {count}")
        items = []
        for index in range(count):
            info = MenuItemInfo(cbSize=C.sizeof(MenuItemInfo), fMask=0x40 | 0x02 | 0x01 | 0x100)
            require(self.user.GetMenuItemInfoW(menu, index, True, C.byref(info)), "menu item metadata query failed")
            require(0 < info.cch <= 256 and not info.hSubMenu and not info.fType & 0x800,
                    "menu item must be a bounded plain command")
            buffer = C.create_unicode_buffer(info.cch+1)
            info.dwTypeData = C.cast(buffer, W.LPWSTR)
            info.cch = len(buffer)
            require(self.user.GetMenuItemInfoW(menu, index, True, C.byref(info)), "menu item text query failed")
            require(0 < info.wID <= 0xFFFF and not info.fState & 0x3, "native menu command has invalid ID or is disabled")
            items.append({"text": buffer.value, "id": info.wID})
        require({item["text"] for item in items} == set(COMMANDS), "native popup text does not match all three allowed commands")
        require(len({item["id"] for item in items}) == len(COMMANDS), "native command IDs are not unique")
        return items


class Guard:
    def __init__(self, native, args):
        self.native, self.args = native, args
        self.exe = args.exe.resolve(strict=True)
        self.directory = args.data_dir.resolve(strict=True)
        root = ROOT.resolve(strict=True)
        require(self.exe.is_relative_to(root) and self.directory.is_relative_to(root), "EXE and physical data-dir must stay inside the D: task cache")
        require(self.exe.is_file() and self.directory.is_dir(), "target executable/data-dir types are invalid")
        self.created = datetime.fromisoformat(args.created)
        require(self.created.tzinfo is not None, "expected creation timestamp must have an explicit timezone")
        self.created = self.created.astimezone(timezone.utc).isoformat(timespec="microseconds")
        self.expected_hash = args.sha256.lower()
        require(len(self.expected_hash) == 64 and all(c in "0123456789abcdef" for c in self.expected_hash), "expected SHA-256 must contain 64 hexadecimal digits")
        # Query/synchronize rights only: the helper never obtains terminate rights.
        self.handle = native.kernel.OpenProcess(0x00100000 | 0x1000, False, args.pid)
        require(self.handle, "cannot open target process for read-only identity and exit waiting")
        try:
            self.revalidate()
        except BaseException:
            self.close()
            raise

    def close(self):
        if self.handle:
            self.native.kernel.CloseHandle(self.handle)
            self.handle = None

    def revalidate(self):
        require(self.native.kernel.WaitForSingleObject(self.handle, 0) == 0x102, "original target process has already exited")
        process = psutil.Process(self.args.pid)
        require(timestamp(process.create_time()) == self.created, "PID creation time differs from the expected process")
        require(Path(process.exe()).resolve(strict=True) == self.exe, "process physical executable differs")
        command = process.cmdline()
        require(command and data_argument(command) == self.directory, "process explicit physical --data-dir differs")
        require(sha256(self.exe) == self.expected_hash, "target executable SHA-256 differs")
        require(timestamp(psutil.Process(self.args.pid).create_time()) == self.created
                and self.native.kernel.WaitForSingleObject(self.handle, 0) == 0x102,
                "target process changed during identity verification")
        return {"pid": self.args.pid, "created_utc": self.created, "exe": str(self.exe),
                "sha256": self.expected_hash, "data_dir": str(self.directory), "command_line": command}

    def wait_for_exit(self, timeout):
        result = self.native.kernel.WaitForSingleObject(self.handle, int(timeout*1000))
        require(result == 0, "verified process did not exit before timeout; it was not killed")
        code = W.DWORD()
        require(self.native.kernel.GetExitCodeProcess(self.handle, C.byref(code)), "cannot retrieve original process exit code")
        require(code.value == 0, f"verified process exited with code {code.value}")
        return code.value

    def exited(self):
        result = self.native.kernel.WaitForSingleObject(self.handle, 0)
        require(result in (0, 0x102), "cannot query original process wait handle")
        return result == 0


def run(args):
    require(os.name == "nt", "this helper requires Windows")
    native = Native()
    guard = Guard(native, args)
    report = {"action": args.action, "pid": args.pid, "actions": []}
    try:
        report.update(guard.revalidate())
        tray, thread = native.tray(args.pid)
        report.update(action=args.action, tray_hwnd=tray, tray_hwnd_hex=hex(tray), tray_thread_id=thread)
        if args.action == "inspect":
            # Deliberately no PostMessage/SendMessage/MN_GETHMENU on this path.
            info = native.gui(thread)
            report.update(messages_sent=0, menu_owner_hwnd=int(info.hwndMenuOwner or 0),
                          popup_hwnds=native.windows("#32768", args.pid, thread))
            return report

        if args.action == "open":
            guard.revalidate()
            require(native.tray(args.pid) == (tray, thread), "tray changed before open action")
            post(native, tray, WM_USER_TRAYICON, 0, WM_RBUTTONUP, report, "WM_USER_TRAYICON/WM_RBUTTONUP")
            deadline = time.monotonic()+3
            while True:
                try:
                    native.popup(args.pid, tray, thread)
                    break
                except RuntimeError:
                    require(time.monotonic() < deadline, "verified tray popup did not appear")
                    time.sleep(.05)
            guard.revalidate()
            report["menu"] = native.menu(args.pid, tray, thread)
            return report

        require(args.command in COMMANDS, "menu-command requires one of the three exact allowed texts")
        first = native.menu(args.pid, tray, thread)
        guard.revalidate()
        require(native.tray(args.pid) == (tray, thread), "tray changed before menu command")
        current = native.menu(args.pid, tray, thread)
        require(current == first, "popup/owner/native menu changed before command")
        actual_id = next(item["id"] for item in current["items"] if item["text"] == args.command)
        require(native.window_identity(current["owner_hwnd"]) == (args.pid, thread), "menu owner process/thread changed")
        report.update(command=args.command, actual_command_id=actual_id, menu=current)
        # A direct WM_COMMAND activates muda's item, but does not end the
        # TrackPopupMenu modal loop. Cancel that verified owner's menu first.
        post(native, current["owner_hwnd"], WM_CANCELMODE, 0, 0, report, "WM_CANCELMODE")
        deadline = time.monotonic()+3
        while True:
            if guard.exited():
                require(args.command == "退出", "original process exited during menu cancellation")
                report.update(command_disposition="pending-exit-completed-after-menu-cancel",
                              exit_code=guard.wait_for_exit(args.exit_timeout))
                return report
            info = native.gui(thread)
            popups = native.windows("#32768", args.pid, thread)
            if not info.hwndMenuOwner and not popups:
                break
            require(time.monotonic() < deadline, "verified native menu did not finish cancellation")
            time.sleep(.025)
        report["menu_cancelled"] = True
        # The popup is now gone; re-read the still-owned HMENU directly rather
        # than sending MN_GETHMENU to a destroyed/reused popup HWND.
        guard.revalidate()
        require(native.tray(args.pid) == (tray, thread), "tray changed after menu cancellation")
        require(not native.gui(thread).hwndMenuOwner and not native.windows("#32768", args.pid, thread),
                "a native menu reopened before command dispatch")
        require(native.menu_items(current["menu_handle"]) == current["items"], "native HMENU commands changed after cancellation")
        require(native.window_identity(current["owner_hwnd"]) == (args.pid, thread), "owner changed before command dispatch")
        post(native, current["owner_hwnd"], WM_COMMAND, actual_id, 0, report, "WM_COMMAND")
        report["command_disposition"] = "posted-after-verified-menu-cancel"
        if args.command == "退出":
            report["exit_code"] = guard.wait_for_exit(args.exit_timeout)
        return report
    except Exception as error:
        # A previously dispatched Exit can complete as cancellation unwinds
        # TrackPopupMenu. Judge that original held process handle, never a
        # fresh PID/HWND, before treating a racing query as an action failure.
        cancelled = any(action["message_id"] == WM_CANCELMODE and action["posted"] for action in report["actions"])
        if args.command == "退出" and cancelled and native.kernel.WaitForSingleObject(guard.handle, 250) == 0:
            report.update(command_disposition="pending-exit-completed-after-menu-cancel",
                          exit_code=guard.wait_for_exit(args.exit_timeout))
            return report
        raise ActionFailure(error, report) from error
    finally:
        guard.close()


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", nargs="?", default="inspect", choices=["inspect", "open", "menu-command"])
    parser.add_argument("--pid", type=int, required=True)
    parser.add_argument("--created", required=True, help="Expected PID creation time as timezone-aware ISO-8601")
    parser.add_argument("--exe", type=Path, required=True)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--data-dir", type=Path, required=True)
    parser.add_argument("--command", choices=COMMANDS)
    parser.add_argument("--exit-timeout", type=float, default=20)
    args = parser.parse_args()
    require(args.pid > 0 and 0 < args.exit_timeout <= 120, "PID and bounded timeout must be positive")
    require((args.action == "menu-command") == (args.command is not None), "--command is only accepted with menu-command")
    try:
        result = run(args)
        result["observed_at_utc"] = datetime.now(timezone.utc).isoformat()
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return 0
    except Exception as error:
        report = getattr(error, "report", {"action": args.action, "pid": args.pid, "actions": []})
        report.update(error=f"{type(error).__name__}: {error}",observed_at_utc=datetime.now(timezone.utc).isoformat())
        print(json.dumps(report, ensure_ascii=False))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
