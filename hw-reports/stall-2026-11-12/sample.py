"""Sample a child process's threads, gnome-shell's threads and the GPU's sysfs.

python3 -I sample.py [--no-sysfs] OUTDIR -- CMD...

Starts CMD as its child (stdin from /dev/null, stderr passed through),
samples until the child exits, writes its files into OUTDIR and exits
with the child's exit code. Standard library only.
"""

import os
import re
import resource
import subprocess
import sys
import threading
import time

GPU = "/sys/bus/pci/devices/0000:0c:00.0"
RENDER = "/dev/dri/renderD128"
CS = re.compile(r":cs\d+$")
WAITS = {7, 232, 271, 281, 441}  # poll, epoll_wait, ppoll, epoll_pwait, epoll_pwait2
IOCTL = 16


def args():
    a = sys.argv[1:]
    sysfs = True
    if a and a[0] == "--no-sysfs":
        sysfs, a = False, a[1:]
    if len(a) < 3 or a[1] != "--":
        sys.exit("usage: sample.py [--no-sysfs] OUTDIR -- CMD...")
    return sysfs, a[0], a[2:]


def opened(path):
    try:
        return os.open(path, os.O_RDONLY)
    except OSError:
        return None


def read(fd):
    try:
        return os.pread(fd, 4096, 0).decode("utf-8", "replace")
    except OSError:
        return None


def close(fd):
    if fd is not None:
        try:
            os.close(fd)
        except OSError:
            pass


def stat_fields(text):
    """The fields after the comm's closing parenthesis: [0] is field 3."""
    return text[text.rindex(")") + 2:].split()


class Thread:
    def __init__(self, pid, tid, full):
        base = f"/proc/{pid}/task/{tid}"
        self.pid, self.tid = pid, tid
        self.comm = (read_once(f"{base}/comm") or "?").strip()
        self.stat = opened(f"{base}/stat")
        self.wchan = opened(f"{base}/wchan")
        self.syscall = opened(f"{base}/syscall") if full else None
        self.status = opened(f"{base}/status") if full else None

    def close(self):
        for fd in (self.stat, self.wchan, self.syscall, self.status):
            close(fd)


def read_once(path):
    try:
        with open(path) as f:
            return f.read()
    except OSError:
        return None


def tids(pid):
    try:
        return {int(t) for t in os.listdir(f"/proc/{pid}/task")}
    except OSError:
        return set()


def gnome_shell():
    uid = os.getuid()
    for p in os.listdir("/proc"):
        if not p.isdigit():
            continue
        try:
            if os.stat(f"/proc/{p}").st_uid != uid:
                continue
            if (read_once(f"/proc/{p}/comm") or "").strip() == "gnome-shell":
                return int(p)
        except OSError:
            continue
    return None


def fd_targets(pid):
    out = {}
    try:
        for fd in os.listdir(f"/proc/{pid}/fd"):
            try:
                out[fd] = os.readlink(f"/proc/{pid}/fd/{fd}")
            except OSError:
                pass
    except OSError:
        pass
    return out


def copy_fdinfo(outdir, name):
    """Copy every renderD128 fd's fdinfo of every process of this user."""
    d = os.path.join(outdir, name)
    os.makedirs(d, exist_ok=True)
    uid = os.getuid()
    for p in os.listdir("/proc"):
        if not p.isdigit():
            continue
        try:
            if os.stat(f"/proc/{p}").st_uid != uid:
                continue
            comm = (read_once(f"/proc/{p}/comm") or "?").strip().replace("/", "_").replace(" ", "_")
            for fd in os.listdir(f"/proc/{p}/fd"):
                try:
                    if os.readlink(f"/proc/{p}/fd/{fd}") != RENDER:
                        continue
                except OSError:
                    continue
                text = read_once(f"/proc/{p}/fdinfo/{fd}")
                if text is not None:
                    with open(os.path.join(d, f"{p}-{comm}-{fd}"), "w") as f:
                        f.write(text)
        except OSError:
            continue


def main():
    sysfs, outdir, cmd = args()
    os.makedirs(outdir, exist_ok=True)
    child = subprocess.Popen(cmd, stdin=subprocess.DEVNULL)
    pid = child.pid
    t0 = time.monotonic_ns()
    shell = gnome_shell()

    threads = {}  # (pid, tid) -> Thread
    focus = []  # Threads sampled every 2 ms
    fds = {}
    gpu_files = {}
    if sysfs:
        for name in ["gpu_busy_percent", "pp_dpm_sclk", "pp_dpm_mclk", "pp_dpm_vclk",
                     "pp_dpm_dclk", "pp_dpm_pcie", "mem_info_vram_used", "mem_info_gtt_used"]:
            gpu_files[name] = opened(f"{GPU}/{name}")

    rows2, rows10, rows25, rowsf, rowsv, marks = [], [], [], [], [], []
    focus_comms = set()
    late, longest = 0, 0
    tick = 2_000_000
    nxt = t0
    n = 0
    marked = set()
    copiers = []

    def rescan():
        nonlocal focus
        live = set()
        for p, full in ((pid, True), (shell, False)):
            if p is None:
                continue
            for t in tids(p):
                live.add((p, t))
                if (p, t) not in threads:
                    threads[(p, t)] = Thread(p, t, full)
        for k in list(threads):
            if k not in live:
                threads.pop(k).close()
        focus = [th for (p, t), th in threads.items() if p == pid and (
            t == pid or th.comm in ("decode-v0", "upload") or CS.search(th.comm))]
        for th in focus:
            focus_comms.add(th.comm)

    def gpu_level(name):
        fd = gpu_files.get(name)
        if fd is None:
            return "na"
        text = read(fd)
        if text is None:
            close(fd)
            gpu_files[name] = None
            return "na"
        if name in ("gpu_busy_percent", "mem_info_vram_used", "mem_info_gtt_used"):
            return text.strip()
        for line in text.splitlines():
            if line.rstrip().endswith("*"):
                return line.split(":")[0].strip()
        return "?"

    rescan()
    fds = fd_targets(pid)
    while child.poll() is None:
        now = time.monotonic_ns()
        if now < nxt:
            time.sleep((nxt - now) / 1e9)
            now = time.monotonic_ns()
        lag = now - nxt
        if lag > 1_000_000:
            late += 1
            longest = max(longest, lag)
        nxt += tick
        if nxt < now:  # skip ticks we can no longer make
            nxt = now + tick - (now - t0) % tick
        wall = time.time_ns()
        n += 1

        if n % 50 == 0:
            rescan()
            fds = fd_targets(pid)
        # every 2 ms: the focus threads
        for th in focus:
            st = read(th.stat)
            if st is None:
                continue
            state = stat_fields(st)[0]
            wc = (read(th.wchan) or "").strip() or "0"
            sc = (read(th.syscall) or "").split()
            s0 = sc[0] if sc else "?"
            s1 = sc[1] if len(sc) > 1 else ""
            s2 = sc[2] if len(sc) > 2 else ""
            target = ""
            if th.tid == pid and s0.lstrip("-").isdigit() and (int(s0) == IOCTL or int(s0) in WAITS) and s1:
                try:
                    target = fds.get(str(int(s1, 16)), "")
                except ValueError:
                    target = ""
            rows2.append((now, wall, th.tid, th.comm, state, wc, s0, s1, s2, target))
        # every 10 ms: every thread of the child and of gnome-shell
        if n % 5 == 0:
            for (p, t), th in list(threads.items()):
                st = read(th.stat)
                if st is None:
                    continue
                rows10.append((now, wall, p, t, th.comm, stat_fields(st)[0],
                               (read(th.wchan) or "").strip() or "0"))
        # every 25 ms (about: every 12th or 13th tick): the GPU
        if sysfs and (n * 2) % 25 < 2:
            rows25.append((now, wall) + tuple(gpu_level(x) for x in
                          ["gpu_busy_percent", "pp_dpm_sclk", "pp_dpm_mclk", "pp_dpm_vclk",
                           "pp_dpm_dclk", "pp_dpm_pcie"]))
        # every 100 ms: faults and memory
        if n % 50 == 25:
            for (p, t), th in list(threads.items()):
                if p != pid:
                    continue
                st = read(th.stat)
                if st is None:
                    continue
                f = stat_fields(st)
                status = read(th.status) or ""
                vol = nonvol = ""
                for line in status.splitlines():
                    if line.startswith("voluntary_ctxt_switches"):
                        vol = line.split()[1]
                    elif line.startswith("nonvoluntary_ctxt_switches"):
                        nonvol = line.split()[1]
                rowsf.append((now, wall, t, th.comm, f[7], f[9], f[11], f[12], vol, nonvol))
            if sysfs:
                rowsv.append((now, wall, gpu_level("mem_info_vram_used"), gpu_level("mem_info_gtt_used")))
        # at 6 s and 16 s: fdinfo
        for at in (6, 16):
            if at not in marked and now - t0 >= at * 1_000_000_000:
                marked.add(at)
                # On a thread of its own, so the 2 ms ticks go on meanwhile.
                def copier(at=at):
                    m0, w0 = time.monotonic_ns(), time.time_ns()
                    copy_fdinfo(outdir, f"fdinfo-{at}")
                    marks.append((m0, w0, at, time.monotonic_ns() - m0))
                copiers.append(threading.Thread(target=copier))
                copiers[-1].start()

    code = child.wait()
    for c in copiers:
        c.join()
    ru = resource.getrusage(resource.RUSAGE_SELF)

    def write(name, header, rows):
        with open(os.path.join(outdir, name), "w") as f:
            f.write("\t".join(header) + "\n")
            for r in rows:
                f.write("\t".join(str(x) for x in r) + "\n")

    write("threads.tsv", ["mono_ns", "wall_ns", "tid", "comm", "state", "wchan", "sc_nr", "sc_a1", "sc_a2", "fd_target"], rows2)
    write("all.tsv", ["mono_ns", "wall_ns", "pid", "tid", "comm", "state", "wchan"], rows10)
    if sysfs:
        write("gpu.tsv", ["mono_ns", "wall_ns", "busy", "sclk", "mclk", "vclk", "dclk", "pcie"], rows25)
        write("vram.tsv", ["mono_ns", "wall_ns", "vram_used", "gtt_used"], rowsv)
    write("faults.tsv", ["mono_ns", "wall_ns", "tid", "comm", "minflt", "majflt", "utime", "stime", "vol_ctxt", "nonvol_ctxt"], rowsf)
    write("marks.tsv", ["mono_ns", "wall_ns", "at_s", "took_ns"], marks)
    with open(os.path.join(outdir, "sampler.txt"), "w") as f:
        f.write(f"cpu_user_s {ru.ru_utime:.3f}\ncpu_sys_s {ru.ru_stime:.3f}\n")
        f.write(f"ticks {n}\nlate_ticks_over_1ms {late}\nlongest_late_ms {longest / 1e6:.3f}\n")
        f.write(f"child_exit {code}\nsysfs {sysfs}\ngnome_shell_pid {shell}\n")
        f.write(f"focus_comms {' '.join(sorted(focus_comms))}\n")
    sys.exit(code if code >= 0 else 128 - code)


if __name__ == "__main__":
    main()
