#!/usr/bin/env python3
"""Check the Rust monitor; retain evidence and emit a concrete repair plan on failure."""
from __future__ import annotations
import argparse, fcntl, http.server, json, math, os, pty, re, select, signal, struct
import subprocess, termios, threading, time, unicodedata
from datetime import datetime, timezone
from pathlib import Path
ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / 'target/release/tabbyapi-monitor'
FIXTURE = (ROOT / 'tests/fixtures/metrics.prom').read_text()

class Fixture:
    def __init__(self, mode='ok'):
        self.mode, self.count, self.authorized = mode, 0, False
        owner = self
        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                status = 200
                if self.path == '/metrics':
                    owner.count += 1
                    body = FIXTURE
                    if owner.mode == 'auth':
                        owner.authorized = self.headers.get('Authorization') == 'Bearer fixture-test-key'
                        if not owner.authorized: status = 401
                    elif owner.mode == 'unavailable': status = 503
                    elif owner.mode == 'bad-schema': body = body.replace('version="1"', 'version="9"')
                    elif owner.mode == 'malformed': body += '\ntabbyapi_output_tokens_total not-a-number\n'
                    elif owner.mode == 'oversized': body += '#' + 'x' * 1_048_577
                    elif owner.mode == 'timeout': time.sleep(.4)
                    elif owner.mode == 'missing': body = '\n'.join(x for x in body.splitlines() if not x.startswith('tabbyapi_kv_cache_used_tokens '))
                    elif owner.mode == 'zero': body = re.sub(r'tabbyapi_kv_cache_used_tokens [^\n]+', 'tabbyapi_kv_cache_used_tokens 0', body)
                elif self.path == '/v1/model': body = '{"id":"fixture-model","parameters":{"max_seq_len":262144},"chat_template":"DO-NOT-RETAIN-TEMPLATE"}'
                elif self.path == '/health': body = '{"status":"healthy"}'
                else: status, body = 404, ''
                payload = body.encode()
                self.send_response(status); self.send_header('Content-Length', str(len(payload))); self.end_headers()
                try: self.wfile.write(payload)
                except (BrokenPipeError, ConnectionResetError): pass
            def log_message(self, *_): pass
        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.server.daemon_threads = True
        self.url = f'http://127.0.0.1:{self.server.server_port}'
    def __enter__(self):
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True); self.thread.start(); return self
    def __exit__(self, *_): self.server.shutdown(); self.server.server_close(); self.thread.join(2)

class Screen:
    """Decode the cursor/erase subset emitted by crossterm, for frame assertions."""
    def __init__(self, cols, rows):
        self.cols, self.rows, self.x, self.y = cols, rows, 0, 0
        self.cells = [[' '] * cols for _ in range(rows)]
        self.pending = ''
    def feed(self, text):
        text = self.pending + text; self.pending = ''; i = 0
        while i < len(text):
            c = text[i]
            if c == '\x1b':
                if i+1 >= len(text): self.pending = text[i:]; break
                if text[i+1] == '[':
                    m = re.match(r'\x1b\[([0-?]*)([ -/]*)([@-~])', text[i:])
                    if not m: self.pending = text[i:]; break
                    raw, _, code = m.groups(); i += len(m.group(0))
                    if raw.startswith('?'): continue
                    args = [int(v) if v else 0 for v in raw.split(';')]; a = args[0] or 1
                    if code in ('H','f'): self.y = (args[0] or 1)-1; self.x = (args[1] if len(args)>1 and args[1] else 1)-1
                    elif code == 'G': self.x = a-1
                    elif code == 'd': self.y = a-1
                    elif code == 'A': self.y -= a
                    elif code == 'B': self.y += a
                    elif code == 'C': self.x += a
                    elif code == 'D': self.x -= a
                    elif code == 'J' and args[0] in (2,3): self.cells = [[' '] * self.cols for _ in range(self.rows)]
                    elif code == 'K' and 0 <= self.y < self.rows:
                        start, end = (0,self.cols) if args[0]==2 else (0,self.x+1) if args[0]==1 else (self.x,self.cols)
                        for x in range(max(0,start),min(end,self.cols)): self.cells[self.y][x] = ' '
                    self.x = max(0,min(self.x,self.cols-1)); self.y = max(0,min(self.y,self.rows-1)); continue
                i += 2; continue
            if c == '\r': self.x = 0
            elif c == '\n': self.y = min(self.rows-1,self.y+1)
            elif c == '\b': self.x = max(0,self.x-1)
            elif c >= ' ' and not unicodedata.combining(c):
                if self.x >= self.cols: self.x=0; self.y=min(self.rows-1,self.y+1)
                self.cells[self.y][self.x] = c
                self.x += 2 if unicodedata.east_asian_width(c) in ('W','F') else 1
            i += 1
    def text(self): return '\n'.join(''.join(row).rstrip() for row in self.cells)

class TUI:
    def __init__(self, argv, cols=120, rows=60):
        self.master, self.slave = pty.openpty(); self.original = termios.tcgetattr(self.slave)
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack('HHHH', rows,cols,0,0))
        env = dict(os.environ, TERM='xterm-256color', COLORTERM='truecolor')
        env.pop('NO_COLOR', None)  # Explicit color-capable acceptance terminal.
        self.proc = subprocess.Popen([str(BINARY)]+argv, stdin=self.slave, stdout=self.slave, stderr=self.slave, env=env, start_new_session=True)
        self.screen, self.raw = Screen(cols,rows), bytearray()
    def drain(self, duration=.3):
        deadline = time.monotonic()+duration
        while time.monotonic()<deadline:
            ready,_,_ = select.select([self.master],[],[],min(.05,max(0,deadline-time.monotonic())))
            if ready:
                try: data = os.read(self.master,65536)
                except OSError: break
                if not data: break
                self.raw.extend(data)
                # Bytes emitted by a single terminal write may be split at UTF-8 boundaries.
                import codecs
                if not hasattr(self,'decoder'): self.decoder=codecs.getincrementaldecoder('utf-8')('replace')
                self.screen.feed(self.decoder.decode(data))
                if len(self.raw)>4_000_000: raise AssertionError('unexpected terminal output growth')
        return self.screen.text()
    def key(self, key, delay=.25): os.write(self.master,key); return self.drain(delay)
    def close(self, sig=None):
        if self.proc.poll() is None:
            if sig: self.proc.send_signal(sig)
            else: os.write(self.master,b'q')
        self.drain(.3)
        try: self.proc.wait(timeout=2)
        except subprocess.TimeoutExpired: self.proc.kill(); self.proc.wait(); raise AssertionError('interactive exit timed out')
        self.restored = termios.tcgetattr(self.slave)==self.original
        os.close(self.slave); os.close(self.master)
        return self.proc.returncode

class Runner:
    def __init__(self,args):
        self.args=args; self.gates=[]
        stamp=datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%S.%fZ')
        self.out=args.output or ROOT/'.state/acceptance'/stamp; self.out.mkdir(parents=True,exist_ok=False)
    def check(self,name,action,repair):
        start=time.monotonic()
        try:
            extra=action() or {}; self.gates.append(dict(name=name,status='pass',duration_seconds=round(time.monotonic()-start,3),**extra))
        except Exception as exc:
            self.gates.append(dict(name=name,status='fail',duration_seconds=round(time.monotonic()-start,3),detail=str(exc) or type(exc).__name__,repair=repair))
    def command(self,name,args,env=None):
        p=subprocess.run(args,cwd=ROOT,env=env,text=True,capture_output=True,timeout=self.args.timeout)
        (self.out/(name+'.stdout.txt')).write_text(p.stdout); (self.out/(name+'.stderr.txt')).write_text(p.stderr)
        return p
    def json(self,name,args,success=True,env=None):
        p=self.command(name,[str(BINARY),'--once','--json','--no-gpu']+args,env)
        assert (p.returncode==0)==success, f'exit {p.returncode}; expected success={success}'
        value=json.loads(p.stdout)
        assert 'DO-NOT-RETAIN-TEMPLATE' not in p.stdout and 'fixture-test-key' not in p.stdout, 'private source data retained'
        return value
    def run(self):
        def test():
            p=self.command('cargo-test',['cargo','test','--locked']); assert p.returncode==0,'cargo tests failed'
        self.check('cargo_tests',test,'Fix the retained Cargo test failures; rerun affected tests and this gate.')
        def build():
            p=self.command('cargo-build',['cargo','build','--release','--locked'])
            assert p.returncode==0, 'locked release build failed'
            return {'bytes':BINARY.stat().st_size}
        self.check('release_binary',build,'Build the locked release executable.')
        if self.gates[-1]['status'] != 'pass':
            return self.finish()  # Never validate an older binary after a failed build.
        if not BINARY.is_file(): return self.finish()
        def mock():
            v=self.json('mock',['--mock']); assert v['mock'] and v['snapshot']['loaded']==1
        self.check('mock_json',mock,'Repair --mock --once --json and explicit synthetic labelling.')
        with Fixture() as fixture:
            def http():
                v=self.json('http',['--url',fixture.url]); assert v['connected'] and v['metadata']['model']=='fixture-model'; assert v['snapshot']['decode']==1
            self.check('http_snapshot',http,'Repair schema-v1 parsing and the filtered model metadata fetch.')
            def missing_zero():
                with Fixture('missing') as missing, Fixture('zero') as zero:
                    a=self.json('missing',['--url',missing.url]); b=self.json('zero',['--url',zero.url]); assert a['snapshot']['used'] is None and b['snapshot']['used']==0
            self.check('missing_vs_zero',missing_zero,'Preserve absent cache occupancy as null, and measured zero as 0.')
            for mode in ['unavailable','auth','bad-schema','malformed','oversized','timeout']:
                def reject(mode=mode):
                    with Fixture(mode) as bad:
                        v=self.json(mode,['--url',bad.url,'--timeout','0.1'],False); assert not v['connected'] and v['error']
                self.check('reject_'+mode,reject,'Reject failed/invalid/oversized metrics with nonzero status and sanitized error JSON.')
            def auth():
                with Fixture('auth') as server:
                    env=dict(os.environ,TABBYAPI_API_KEY='fixture-test-key'); v=self.json('authorized',['--url',server.url],env=env); assert server.authorized and v['connected']
            self.check('authenticated_get',auth,'Forward bearer auth on read-only requests without retaining the key.')
            def fleet_errors():
                with Fixture('unavailable') as bad:
                    v=self.json('fleet-errors',['--url',fixture.url,'--url',bad.url],False); assert len(v)==2 and v[0]['connected'] and not v[1]['connected']
            self.check('fleet_failure_isolation',fleet_errors,'Keep per-target snapshots/errors separate and reject partial one-shot failure.')
            def controls():
                tui=TUI(['--url',fixture.url,'--interval','0.1','--no-gpu']); frames=[]
                try:
                    frame=tui.drain(.8)
                    deadline=time.monotonic()+4
                    while 'fixture-model' not in frame and time.monotonic()<deadline: frame=tui.drain(.2)
                    assert 'CONCURRENCY' in frame and 'fixture-model' in frame, 'initial frame missing model/concurrency'
                    frames.append(frame); frame=tui.key(b'p',.4); assert 'PAUSED' in frame
                    before=fixture.count;tui.drain(.4);assert fixture.count==before,'pause still polls'
                    frame=tui.key(b'p',.4); assert 'Polling resumed' in frame and fixture.count>before
                    frame=tui.key(b'-');assert '0.2s' in frame
                    frame=tui.key(b'+');assert '0.1s' in frame
                    frame=tui.key(b'r');assert 'Session reset' in frame
                    frame=tui.key(b'g');assert 'GPU shown' in frame
                    frame=tui.key(b'g');assert 'GPU hidden' in frame
                    frame=tui.key(b't');assert 'Feed hidden' in frame
                    frame=tui.key(b't');assert 'Feed shown' in frame
                    frame=tui.key(b'd');assert 'Details shown' in frame and 'LATENCY' in frame
                    frame=tui.key(b'd');assert 'Details hidden' in frame and 'LATENCY' not in frame
                    frames.append(frame)
                    fixture.mode='unavailable';frame=tui.drain(.5);assert 'STALE' in frame and 'fixture-model' in frame;frames.append(frame)
                    fixture.mode='ok';frame=tui.drain(.5);assert 'CONNECTED' in frame;frames.append(frame)
                finally:
                    code=tui.close();(self.out/'controls.ansi').write_bytes(tui.raw);(self.out/'controls.frames.txt').write_text('\n\n'.join(frames))
                assert code==0 and tui.restored,'terminal modes not restored'
                assert b'38;2;203;166;247' in tui.raw and b'48;2;30;30;46' in tui.raw,'Mocha/Mauve RGB missing'
            self.check('controls_staleness_theme_restore',controls,'Inspect controls frames and fix polling, local key state, theme, or terminal cleanup.')
        def fleet():
            tui=TUI(['--mock','--no-gpu','--url','http://alpha.invalid','--url','http://beta.invalid'])
            frames=[]
            try:
                frame=tui.drain(.4);assert 'TABBYAPI FLEET' in frame;frames.append(frame)
                tui.key(b'j');frame=tui.key(b'\r');assert 'EXLLAMAV3' in frame and 'beta.invalid' in frame;frames.append(frame)
                frame=tui.key(b'\x1b');assert 'TABBYAPI FLEET' in frame
                tui.key(b'k');frame=tui.key(b'\r');assert 'alpha.invalid' in frame
            finally:
                code=tui.close();(self.out/'fleet.frames.txt').write_text('\n\n'.join(frames));(self.out/'fleet.ansi').write_bytes(tui.raw)
            assert code==0 and tui.restored
        self.check('fleet_navigation',fleet,'Repair fleet selection, Enter detail, and Esc overview, retaining per-target state.')
        def narrow():
            tui=TUI(['--mock','--no-gpu'],70,24)
            try:
                frame=tui.drain(.3); assert 'CONCURRENCY' in frame
                reached=set()
                for _ in range(8):
                    frame=tui.key(b'\x1b[6~')
                    reached.update(name for name in ('THROUGHPUT','CACHE','TEE') if name in frame)
                assert reached == {'THROUGHPUT','CACHE','TEE'}, reached
                (self.out/'narrow.frame.txt').write_text(frame)
                fcntl.ioctl(tui.slave,termios.TIOCSWINSZ,struct.pack('HHHH',40,120,0,0));tui.proc.send_signal(signal.SIGWINCH);tui.drain(.2)
            finally: code=tui.close();(self.out/'narrow.ansi').write_bytes(tui.raw)
            assert code==0 and tui.restored
        self.check('narrow_scroll_resize',narrow,'Ensure all narrow-screen panels remain reachable by scrolling and resizing never crashes.')
        def sigterm():
            tui=TUI(['--mock','--no-gpu']);tui.drain(.2);code=tui.close(signal.SIGTERM);assert code==0 and tui.restored,'SIGTERM did not restore terminal cleanly'
        self.check('signal_cleanup',sigterm,'Handle termination signals through the normal terminal restoration path.')
        for label,args in [('mock_active',['--mock','--no-gpu'])]:
            self.check('resources_'+label,lambda args=args:self.resources(args),'Reduce idle redraws/allocations or unbounded state; repeat warmed release measurements.')
        with Fixture() as fixture:
            self.check('resources_static_fixture',lambda:self.resources(['--url',fixture.url,'--no-gpu']),'Reduce idle polling/render overhead; repeat warmed static-fixture resource measurements.')
        if self.args.live_endpoint:
            def live():
                v=self.json('live',['--url',self.args.live_endpoint,'--timeout','10']);assert v['connected'] and v['snapshot']['loaded']==1
                return {'model':v['metadata']['model'],'schema_version':1,'scope':'passive monitor GET; no generated inference'}
            self.check('live_stack',live,'Inspect endpoint availability and schema; repeat passive monitor check without inference.')
        else:self.gates.append(dict(name='live_stack',status='not_requested',detail='Use --live-endpoint for separate passive live evidence.'))
        return self.finish()
    def resources(self,args):
        tui=TUI(args); peak=0; begin=end=None
        def measure():
            raw=subprocess.check_output(['ps','-o','rss=,time=','-p',str(tui.proc.pid)],text=True,timeout=2).split()
            parts=raw[1].split(':');seconds=sum(float(n)*60**i for i,n in enumerate(reversed(parts)))
            return int(raw[0])*1024,seconds
        try:
            tui.drain(1.); _,first=measure(); start=time.monotonic(); deadline=start+self.args.sample_seconds
            while time.monotonic()<deadline:
                tui.drain(.2); rss,cpu=measure();peak=max(peak,rss);end=cpu
            cpu_pct=(end-first)/(time.monotonic()-start)*100
        finally:tui.close()
        result=dict(peak_rss_bytes=peak,cpu_interval_percent=round(cpu_pct,3),sample_seconds=self.args.sample_seconds,binary_bytes=BINARY.stat().st_size)
        (self.out/('resources-'+str(len(self.gates))+'.json')).write_text(json.dumps(result,indent=2))
        assert 0<peak<=self.args.max_rss_mib*1024**2,f'RSS {peak/1024**2:.2f} MiB exceeds {self.args.max_rss_mib}'
        assert cpu_pct<=self.args.max_cpu_percent,f'CPU {cpu_pct:.2f}% exceeds {self.args.max_cpu_percent}'
        return result
    def finish(self):
        failed=[g for g in self.gates if g['status']=='fail'];overall='fail' if failed else 'pass'
        report=dict(schema_version=2,overall=overall,scope='dashboard, controls, fleet, serving metrics; passive observations only',gates=self.gates,repair_plan=[dict(gate=g['name'],task=g['repair']) for g in failed])
        (self.out/'report.json').write_text(json.dumps(report,indent=2)+'\n')
        lines=['# Acceptance report','',f'Overall: **{overall.upper()}**','','| Gate | Status | Detail |','|---|---|---|']
        lines += [f"| {g['name']} | {g['status']} | {g.get('detail','Verified; see retained artifacts')} |" for g in self.gates]
        lines += ['','## Repair plan','']+[f"- **{g['name']}**: {g['repair']}" for g in failed]
        if not failed:lines+=['No failed gates. Live acceptance is separate when not requested.']
        (self.out/'report.md').write_text('\n'.join(lines)+'\n');print(f"{overall}: {len(failed)} failed; {self.out/'report.md'}")
        for g in failed:print(f"  {g['name']}: {g['detail']}")
        return int(bool(failed))

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--timeout',type=float,default=90);p.add_argument('--sample-seconds',type=float,default=5);p.add_argument('--max-rss-mib',type=float,default=32);p.add_argument('--max-cpu-percent',type=float,default=2);p.add_argument('--live-endpoint');p.add_argument('--output',type=Path);a=p.parse_args()
    if not all(math.isfinite(v) and v>0 for v in [a.timeout,a.sample_seconds,a.max_rss_mib,a.max_cpu_percent]):p.error('timeouts and thresholds must be finite and positive')
    return Runner(a).run()
if __name__=='__main__':raise SystemExit(main())
