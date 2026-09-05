"""Compare model-interaction timing: harness session vs pi session records.

Harness: sessions/better-ui-colors_h1/events.jsonl (second-precision ts).
pi: ~/.pi/agent/sessions/.../2026-08-31T12-48-33-131Z_*.jsonl (millisecond ts).

Both use model Qwen3.8-27B-NVFP4-RTX5090-DSPARK via sglang @ 127.0.0.1:30000.
Usage: uv run python scratch/timestamp_analysis.py <harness_events.jsonl> <pi_session.jsonl>
"""
import json, os, statistics, sys
from collections import defaultdict
from datetime import datetime, timezone


def pctl(a, q):
    a = sorted(a)
    if not a:
        return None
    k = (len(a) - 1) * q
    f = int(k)
    c = min(f + 1, len(a) - 1)
    return a[f] if f == c else a[f] * (c - k) + a[c] * (k - f)


def summarize(name, turns, note=''):
    print(f"\n=== {name} {note} ===")
    print(f"window: {turns[0]['ts']} -> {turns[-1]['ts']}")
    model_turns = [t for t in turns if t['start_kind'] == 'model' and not t.get('user_idle')]
    user_turns = [t for t in turns if t['start_kind'] == 'user']
    mg = [t['model_gap'] for t in model_turns]
    tg = [t['tool_gap'] for t in turns if t['tool_gap'] > 0]
    print(f"turns={len(turns)} (model-driven={len(model_turns)}, user-started={len(user_turns)})")
    if tg:
        print(f"tool exec: n={len(tg)} med={pctl(tg, .5):.1f}s p90={pctl(tg, .9):.1f}s max={max(tg):.0f}s total={sum(tg) / 60:.1f}min")
    if mg:
        print(f"model round-trip gap: med={pctl(mg, .5):.1f}s p50-90={pctl(mg, .9):.1f}s p99={pctl(mg, .99):.1f}s max={max(mg):.0f}s total={sum(mg) / 60:.1f}min")
    if user_turns:
        print('user-started turns:')
        for t in user_turns:
            print(f"  {t['ts']} wait={t['model_gap']:.0f}s in={t.get('intok')} out={t.get('out')}")
    bands = defaultdict(list)
    for t in model_turns:
        if t.get('intok'):
            bands[min(6, t['intok'] // 10000)].append(t)
    print("by input-token band (model-driven turns):")
    for b in sorted(bands):
        ts_ = bands[b]
        gg = [t['model_gap'] for t in ts_]
        out = [t['out'] for t in ts_ if t.get('out')]
        row = f"  {b * 10}k-{b * 10 + 9}k: n={len(ts_)} med={pctl(gg, .5):.1f}s p90={pctl(gg, .9):.1f}s"
        if out:
            row += f" mean_out={statistics.mean(out):.0f}"
        print(row)
    rows = [(t['model_gap'], t['out']) for t in model_turns if t['model_gap'] > 5 and t.get('out')]
    tp = [o / g for g, o in rows]
    if tp:
        print(f"response tok/s (out_tokens/gap, gap>5s): med={pctl(tp, .5):.1f} p10={pctl(tp, .1):.1f} p90={pctl(tp, .9):.1f}")


def analyze_harness(path):
    lines = [json.loads(l) for l in open(path) if l.strip()]
    events = [(datetime.fromisoformat(e['ts'].replace('Z', '+00:00')), e) for e in lines]
    turns = []
    i = 0
    while i < len(events):
        ts, e = events[i]
        if e['type'] == 'assistant_message':
            k = i - 1
            while k >= 0 and events[k][1]['type'] in ('tool_call', 'tool_result'):
                k -= 1
            start_e = events[k][1]
            start_kind = 'user' if start_e['type'] == 'user_message' else 'model'
            tcs = e.get('tool_calls') or []
            ids = {c['id'] for c in tcs}
            last_tool_ts = None
            for m in range(i + 1, len(events)):
                em = events[m][1]
                if em['type'] == 'tool_result' and em['id'] in ids:
                    last_tool_ts = events[m][0]
            u = e.get('usage') or {}
            turns.append(dict(
                ts=e['ts'],
                model_gap=(ts - events[k][0]).total_seconds(),
                tool_gap=(last_tool_ts - ts).total_seconds() if last_tool_ts else 0,
                start_kind=start_kind,
                intok=u.get('input_tokens'), out=u.get('output_tokens'),
                cached=u.get('cached_tokens', 0),
                ntools=len(tcs), stop=e.get('stop_reason'), user_idle=0,
            ))
            i += 1
        else:
            i += 1
    if turns:
        # handoff gap before loop spawn: user idle
        turns[0]['user_idle'] = turns[0]['model_gap']
    summarize('harness better-ui-colors_h1', turns, '(second-precision ts)')
    return turns


def analyze_pi(path):
    raw = [json.loads(l) for l in open(path) if l.strip()]
    entries = [e for e in raw if e['type'] == 'message']

    def tstr(ms):
        return datetime.fromtimestamp(ms / 1000, tz=timezone.utc).strftime('%H:%M:%S')

    turns = []
    prev_ts = None
    prev_kind = None
    for e in entries:
        m = e['message']
        role = m.get('role')
        ts_ms = m['timestamp']
        ts = tstr(ts_ms)
        if role == 'assistant':
            u = m.get('usage') or {}
            start_kind = 'user' if prev_kind == 'user' else 'model'
            gap = (ts_ms - prev_ts) / 1000.0 if prev_ts else 0.0
            turns.append(dict(ts=ts, model_gap=gap, start_kind=start_kind,
                             intok=u.get('input'), out=u.get('output'),
                             reasoning=u.get('reasoning'),
                             tool_gap=0, user_idle=0, stop=m.get('stopReason')))
            prev_ts, prev_kind = ts_ms, 'assistant'
        elif role == 'user':
            prev_ts, prev_kind = ts_ms, 'user'
        elif role == 'toolResult':
            prev_ts, prev_kind = ts_ms, 'toolResult'
    # tool exec: from assistant ts to last toolResult of the group
    a = [t for t in turns if t['stop'] in ('toolUse', None)]
    turns.sort(key=lambda t: t['ts'])
    # recompute tool gaps by walking raw
    msg = []
    for e in entries:
        m = e['message']
        if m.get('role') in ('assistant', 'toolResult'):
            msg.append((m['timestamp'], m.get('role'), m))
    i = 0
    while i < len(msg):
        ts_ms, role, m = msg[i]
        if role == 'assistant':
            ids = {c['id'] for c in m.get('content', []) if isinstance(c, dict) and c.get('type') == 'toolCall'}
            last = None
            for j in range(i + 1, len(msg)):
                if msg[j][1] == 'toolResult' and msg[j][2].get('toolCallId') in ids:
                    last = msg[j][0]
            turn = next((t for t in turns if t['ts'] == tstr(ts_ms)), None)
            if turn is not None:
                turn['tool_gap'] = (last - ts_ms) / 1000.0 if last else 0.0
            i += 1
        else:
            i += 1
    summarize('pi session', turns, '(millisecond ts)')
    return turns


if __name__ == '__main__':
    harness = sys.argv[1]
    pi = sys.argv[2]
    analyze_harness(harness)
    analyze_pi(pi)
