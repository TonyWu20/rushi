"""Band-matched comparison of model round-trip time:
harness sessions/better-ui-colors_h1 vs ALL pi session records.

Harness: assistant_message ts = response completion (1s precision).
  model_time = ts(assistant_i) - ts(last prior tool_result | user_message)
pi: assistant message ts = request start (ms precision).
  model_time = ts(last toolResult) - ts(assistant) - tool_exec_estimate
  (Agent tools: details.durationMs; fast tools: 0.3s allowance each;
   bash groups: excluded (exec unknown); ask_user_question turns: excluded)
"""
import json, os, statistics, glob, sys
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


FAST = {'read', 'ls', 'list', 'edit', 'write', 'grep', 'ffgrep', 'fffind', 'kill'}
WAIT = {'ask_user_question'}


def harness_turns(path):
    lines = [json.loads(l) for l in open(path) if l.strip()]
    events = lines
    out = []
    for i, e in enumerate(events):
        if e['type'] != 'assistant_message':
            continue
        k = i - 1
        while k >= 0 and events[k]['type'] in ('tool_call', 'tool_result'):
            k -= 1
        if events[k]['type'] == 'user_message':
            continue
        gap = (datetime.fromisoformat(e['ts'].replace('Z', '+00:00'))
               - datetime.fromisoformat(events[k]['ts'].replace('Z', '+00:00'))).total_seconds()
        u = e.get('usage') or {}
        out.append(dict(intok=u.get('input_tokens'), out=u.get('output_tokens'), gap=gap,
                        day=e['ts'][:10], src='h1'))
    return out


def pi_turns(paths):
    out = []
    for p in paths:
        raw = [json.loads(l) for l in open(p) if l.strip()]
        msgs = [e for e in raw if e['type'] == 'message']
        results = [e for e in msgs if e['message'].get('role') == 'toolResult']
        day = os.path.basename(p)[:10]
        for e in msgs:
            m = e['message']
            if m.get('role') != 'assistant':
                continue
            u = m.get('usage') or {}
            # pi usage.input is the UNCACHED delta; full context =
            # input + cacheRead + cacheWrite (see pi source: promptTokens).
            ctx = (u.get('input') or 0) + (u.get('cacheRead') or 0) + (u.get('cacheWrite') or 0)
            ot = (u.get('output') or 0) + (u.get('reasoning') or 0)
            tools = [c.get('name') for c in m.get('content', [])
                     if isinstance(c, dict) and c.get('type') == 'toolCall']
            ids = {c.get('id') for c in m.get('content', [])
                   if isinstance(c, dict) and c.get('type') == 'toolCall'}
            if not ids:
                continue
            if any(t in WAIT for t in tools):
                continue
            last, exec_ms, known = 0, 0.0, True
            for r in results:
                rm = r['message']
                if rm.get('toolCallId') in ids:
                    last = max(last, rm['timestamp'])
                    d = rm.get('details') or {}
                    if 'durationMs' in d:
                        exec_ms = max(exec_ms, d['durationMs'])
                    elif rm.get('toolName') in FAST:
                        exec_ms += 0.3
                    else:
                        known = False
            if last == 0 or not known:
                continue
            gap = (last - m['timestamp']) / 1000.0 - exec_ms / 1000.0
            out.append(dict(intok=ctx, out=ot, gap=gap, day=day, src=os.path.basename(p)[:13]))
    return out


def band_row(rows):
    g = [r['gap'] for r in rows if r.get('gap') is not None]
    return dict(n=len(rows), med=pctl(g, .5), p90=pctl(g, .9), max=max(g) if g else None)


def main():
    h1 = harness_turns(sys.argv[1])
    files = sorted(glob.glob(os.path.expanduser('~/.pi/agent/sessions/*/*.jsonl')))
    pt = pi_turns(files)
    print(f"h1 turns: {len(h1)}   pi clean turns: {len(pt)} across {len(files)} files")

    print('\n--- model time by input band ---')
    print('band     h1(n/med/p90)                  pi(n/med/p90)')
    for b in range(0, 7):
        lo, hi = b * 10000, b * 10000 + 9999
        hr = [r for r in h1 if r['intok'] and lo <= r['intok'] < hi]
        pr = [r for r in pt if r['intok'] and lo <= r['intok'] < hi]
        h, p = band_row(hr), band_row(pr)
        hs = f"n={h['n']:3d} med={h['med']:6.1f} p90={h['p90']:6.1f}" if hr else 'n=  0'
        ps = f"n={p['n']:3d} med={p['med']:6.1f} p90={p['p90']:6.1f}" if pr else 'n=  0'
        print(f"{b*10:2d}-{b*10+1}k  {hs:24s}  {ps}")

    print('\n--- latency floor (out < 300 tokens) by input band ---')
    print('band     h1(n/med/p90)                  pi(n/med/p90)')
    for b in range(0, 7):
        lo, hi = b * 10000, b * 10000 + 9999
        hr = [r for r in h1 if r['intok'] and r.get('out') is not None and r['out'] < 300 and lo <= r['intok'] < hi]
        pr = [r for r in pt if r['intok'] and r.get('out') is not None and r['out'] < 300 and lo <= r['intok'] < hi]
        h, p = band_row(hr), band_row(pr)
        hs = f"n={h['n']:3d} med={h['med']:6.1f} p90={h['p90']:6.1f}" if hr else 'n=  0'
        ps = f"n={p['n']:3d} med={p['med']:6.1f} p90={p['p90']:6.1f}" if pr else 'n=  0'
        print(f"{b*10:2d}-{b*10+1}k  {hs:24s}  {ps}")

    print('\n--- matched pairs (|din|<=3k, |dout|<=max(10%,300)) ---')
    ratios = []
    for r in h1:
        if not r['intok'] or r.get('out') is None:
            continue
        cands = [q for q in pt if q['intok'] and abs(q['intok'] - r['intok']) <= 3000
                 and abs(q['out'] - r['out']) <= max(0.1 * r['out'], 300)
                 and 0 < q['gap'] < 300]
        if cands:
            ratios.append(r['gap'] / statistics.median([c['gap'] for c in cands]))
    if ratios:
        rs = sorted(ratios)
        print(f"pairs: {len(ratios)}  median(h1/pi)={pctl(rs, .5):.2f} p25={pctl(rs, .25):.2f} p75={pctl(rs, .75):.2f}")
        print(f"h1 slower: {sum(1 for x in rs if x > 1)}  pi slower: {sum(1 for x in rs if x < 1)}")

    print('\n--- throughput (out+reasoning tokens / model time) ---')
    for name, rows in (('h1', h1), ('pi', pt)):
        tp = [r['out'] / r['gap'] for r in rows if r.get('out') and r['gap'] and r['gap'] > 5]
        if tp:
            print(f"{name}: med={pctl(tp, .5):.1f} p25={pctl(tp, .25):.1f} p75={pctl(tp, .75):.1f} tok/s (n={len(tp)})")

    print('\n--- pi clean turns on 2026-08-31 (same day as h1) ---')
    same = [r for r in pt if r['day'] == '2026-08-31']
    print(f"n={len(same)}")
    for b in range(0, 7):
        lo, hi = b * 10000, b * 10000 + 9999
        rr = [r for r in same if r['intok'] and lo <= r['intok'] < hi]
        if rr:
            g = [r['gap'] for r in rr]
            print(f"  {b*10}-{b*10+1}k: n={len(rr)} med={pctl(g, .5):.1f}s p90={pctl(g, .9):.1f}s")


if __name__ == '__main__':
    main()
