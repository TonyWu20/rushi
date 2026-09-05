import os, pty, select, struct, termios, fcntl, signal, time
REPO = "/home/tony/programming/rust-unix-harness"
BIN = REPO + "/target/debug/tui"
CFG = REPO + "/config.toml"
SESSION = "bash-battle"
CLEAN = "/usr/bin:/bin:/usr/local/bin"
LOG = "/tmp/tui_clean.log"
try: os.unlink(LOG)
except OSError: pass
master, slave = pty.openpty()
fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 120, 0, 0))
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 120, 0, 0))
pid = os.fork()
if pid == 0:
    os.setsid()
    os.dup2(slave,0); os.dup2(slave,1); os.dup2(slave,2)
    for fd in (master,slave): os.close(fd)
    os.environ["PATH"] = CLEAN
    os.environ["TUI_EXT_LOG"] = LOG
    os.execv(BIN, [BIN, SESSION, "--config", CFG])
os.close(slave)
def pump(sec):
    end=time.time()+sec; out=b""
    while time.time()<end:
        r,_,_=select.select([master],[],[],0.1)
        if r:
            try:
                c=os.read(master,65536)
            except OSError:
                break
            if not c: break
            out+=c
    return out
data=pump(10)
raw=data.decode("utf-8","replace")
print("=== ext log ===")
try:
    print(open(LOG).read())
except OSError as e:
    print("(no log)", e)
for name in ["frame","mermaid","notify","statusline"]:
    print(f"{name}: spawn={'spawn '+name in open(LOG).read() if os.path.exists(LOG) else False}")
print("statusline dir pill present:", "ust-unix-harness" in raw)
print("git pill present:", "git:" in raw)
print("model pill present:", "Qwen3.8" in raw)
print("usage pill present:", "in:" in raw)
# quit
os.write(master,b"\x1b"); pump(0.5)
os.write(master,b"q"); pump(0.5)
os.write(master,b"q")
for _ in range(20):
    try:
        p,_=os.waitpid(pid,os.WNOHANG)
        if p: break
    except ChildProcessError: break
    pump(0.2)
try: os.close(master)
except OSError: pass
