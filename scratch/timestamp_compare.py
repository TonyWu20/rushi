"""Final comparison: harness session vs pi session, model-interaction timing.

Semantics (verified in source and data):
- Harness events.jsonl: assistant_message ts = model response COMPLETION.
  tool_call ts ~= tool_result ts at start; tool_result ts = tool completion.
  Second-precision timestamps.
- pi session jsonl: assistant message ts = request START (6-12 ms after the
  prior event). toolResult ts = tool completion (ms precision).
  Agent tool results carry details.durationMs = subagent wall time.

Metrics:
- Harness model time  = ts(assistant) - ts(last prior tool_result | user_message)
- pi model time       = ts(last toolResult of group) - ts(assistant) - tool_exec
"""
import json, os, statistics, sys
from collections import defaultdict
from datetime import datetime, timezone


def parse(ts):
    return datetime.fromisoformat(ts.replace('Z', '+00:00'))


def pctl(a, q):
    a = sorted(a)
    if not a:
        return None
    k = (len(a) - 1) * q
    f = int(k)
    c = min(f + 1, len(a) - 1)
    return a[f] if f == c else a[f] * (c - k) + a[c] * (k - f)


def fmt(v, unit='s'):
    return 'n/a' if v is None else f"{v:.1f}{unit}"


def print_block(name, note, model_gaps, tool_gaps, user_turns, bands, extra=None):
    print(f"\n=== {name} {note} ===")
    if model_gaps:
        g = list(model_gaps)
        print(f"model time (n={len(g)}): med={fmt(pctl(g, .5))} p25={fmt(pctl(g, .25))} p90={fmt(pctl(g, .9))} p99={fmt(pctl(g, .99))} max={fmt(max(g))} total={sum(g) / 60:.1f}min")
    if tool_gaps:
        t = list(tool_gaps)
        print(f"tool exec (n={len(t)}): med={fmt(pctl(t, .5))} p90={fmt(pctl(t, .9))} max={fmt(max(t))} total={sum(t) / 60:.1f}min")
    if user_turns:
        print('user-started turns:')
        for u in user_turns:
            print(f"  {u['ts']} in={u.get('in_tok')} out={u.get('out_tok')}")
    if bands:
        print("model time by input band (model-driven turns):")
        for b in sorted(bands):
            gg = bands[b]
            print(f"  {b * 10}k-{b * 10 + 9}k: n={len(gg)} med={fmt(pctl(gg, .5))} p90={fmt(pctl(gg, .9))}")
    if extra:
        print(extra)


def harness_report(path):
    lines = [json.loads(l) for l in open(path) if l.strip()]
    events = [(parse(e['ts']), e) for e in lines]
    n = len(events)
    model_gaps, tool_gaps, user_turns = [], [], []
    bands = defaultdict(list)
    in_out = []
    for i, (ts, e) in enumerate(events):
        if e['type'] != 'assistant_message':
            continue
        k = i - 1
        while k >= 0 and events[k][1]['type'] in ('tool_call', 'tool_result'):
            k -= 1
        start_ts, start_e = events[k]
        start_kind = 'user' if start_e['type'] == 'user_message' else 'model'
        gap = (ts - start_ts).total_seconds()
        u = e.get('usage') or {}
        it, ot = u.get('input_tokens'), u.get('output_tokens')
        if start_kind == 'user':
            user_turns.append(dict(ts=e['ts'], gap=gap, in_tok=it, out_tok=ot))
        else:
            model_gaps.append(gap)
            if it:
                bands[min(6, it // 10000)].append(gap)
            if it and ot and gap > 5:
                in_out.append((it, ot, gap))
        ids = {c['id'] for c in (e.get('tool_calls') or [])}
        call_ts = last_res = None
        for m in range(i + 1, n):
            em = events[m][1]
            if em['type'] == 'tool_call' and em['id'] in ids and call_ts is None:
                call_ts = events[m][0]
            if em['type'] == 'tool_result' and em['id'] in ids:
                last_res = events[m][0]
        if call_ts and last_res:
            tool_gaps.append((last_res - call_ts).total_seconds())
    tp = [o / g for it, o, g in in_out]
    extra = f"resp tok/s (out/gap, gap>5s): med={fmt(pctl(tp, .5), '')} p10={fmt(pctl(tp, .1), '')}" if tp else ''
    print_block('harness better-ui-colors_h1', '(ts=completion, 1s precision)', model_gaps, tool_gaps, user_turns, bands, extra)
    return model_gaps, tool_gaps


FAST_TOOLS = {'read', 'ls', 'list', 'edit', 'write', 'grep', 'ffgrep', 'fffind', 'kill'}
USER_WAIT_TOOLS = {'ask_user_question'}


def pi_report(path):
    raw = [json.loads(l) for l in open(path) if l.strip()]
    msgs = [e for e in raw if e['type'] == 'message']
    assts = []
    for e in msgs:
        m = e['message']
        if m.get('role') != 'assistant':
            continue
        u = m.get('usage') or {}
        assts.append(dict(
            ts_ms=m['timestamp'],
            it=u.get('input'),
            ot=(u.get('output') or 0) + (u.get('reasoning') or 0),
            tools=[c.get('name') for c in m.get('content', [])
                   if isinstance(c, dict) and c.get('type') == 'toolCall'],
            ids={c.get('id') for c in m.get('content', [])
                 if isinstance(c, dict) and c.get('type') == 'toolCall'},
            stop=m.get('stopReason')))
    results = [e for e in msgs if e['message'].get('role') == 'toolResult']
    model_gaps, tool_gaps = [], []
    bands = defaultdict(list)
    in_out = []
    for a in assts:
        if not a['ids']:
            continue
        if any(t in USER_WAIT_TOOLS for t in a['tools']):
            continue  # tool blocks on user input; gap is user wait, not model
        last_ms, exec_ms, has_agent = 0, 0.0, False
        bash_unknown = False
        for r in results:
            m = r['message']
            if m.get('toolCallId') in a['ids']:
                last_ms = max(last_ms, m['timestamp'])
                d = m.get('details') or {}
                if 'durationMs' in d:
                    exec_ms = max(exec_ms, d['durationMs'])
                    has_agent = True
                elif m.get('toolName') in FAST_TOOLS:
                    exec_ms += 0.3
                else:
                    # bash etc: exec unknown; do not subtract
                    bash_unknown = True
        if last_ms == 0:
            continue
        total = (last_ms - a['ts_ms']) / 1000.0
        model_est = total - exec_ms / 1000.0
        if not bash_unknown:
            model_gaps.append(model_est)
        if a['it'] and not bash_unknown:
            bands[min(6, a['it'] // 10000)].append(model_est)
        if a['it'] and a['ot'] and model_est > 5 and not bash_unknown:
            in_out.append((a['it'], a['ot'], model_est))
    users = [e for e in msgs if e['message'].get('role') == 'user']
    user_turns = []
    for e in users:
        m = e['message']
        ts_s = datetime.fromtimestamp(m['timestamp'] / 1000, tz=timezone.utc).strftime('%H:%M:%S')
        user_turns.append(dict(ts=ts_s, in_tok=None, out_tok=None))
    tp = [o / g for it, o, g in in_out]
    extra = f"resp tok/s (out+reasoning/gap, gap>5s): med={fmt(pctl(tp, .5), '')} p10={fmt(pctl(tp, .1), '')}" if tp else ''
    print_block('pi ' + os.path.basename(path)[:44], '(ts=start, ms precision)', model_gaps, tool_gaps, user_turns, bands, extra)
    return model_gaps, tool_gaps


if __name__ == '__main__':
    harness_report(sys.argv[1])
    for p in sys.argv[2:]:
        pi_report(p)
