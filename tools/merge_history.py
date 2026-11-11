"""Merge frozen, non-overlapping traffic history snapshots into an offline target."""

import argparse
from contextlib import contextmanager
from dataclasses import asdict
import json
import msvcrt
from pathlib import Path
import sqlite3
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from clash_monitor.recovery import HistorySource, RecoveryError, merge_history


@contextmanager
def writer_lock(directory):
    directory.mkdir(parents=True, exist_ok=True)
    with (directory / "collector.lock").open("a+b") as lock:
        lock.seek(0)
        try:
            msvcrt.locking(lock.fileno(), msvcrt.LK_NBLCK, 1)
        except OSError as error:
            raise RecoveryError("采集实例仍在使用目标目录，请从托盘退出后再恢复。") from error
        yield


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", type=Path, required=True, help="离线目标数据库")
    parser.add_argument("--source", type=Path, action="append", required=True, help="冻结快照，可重复传入")
    parser.add_argument("--source-key", action="append", required=True, help="相应原数据库路径，可重复传入")
    args = parser.parse_args()
    if len(args.source) != len(args.source_key):
        parser.error("--source 和 --source-key 必须数量相同并按顺序对应")
    sources = [HistorySource(path, key) for path, key in zip(args.source, args.source_key)]
    try:
        with writer_lock(args.target.expanduser().resolve().parent):
            results = merge_history(args.target, sources)
    except (RecoveryError, OSError, sqlite3.Error) as error:
        print(f"恢复失败：{error}", file=sys.stderr)
        return 1
    print(json.dumps([asdict(result) for result in results], ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
