#!/usr/bin/env python3
"""A1 with local models: Spherecut's MCP tools driven by models served
over an OpenAI-compatible API (llama-swap on huginn), on synthetic media
with planted answers.

For each model and task it starts `spherecut mcp`, gives the model the
server's instructions, SKILL.md and the tools, and runs the chat loop
until the model answers or runs out of steps. Each task asks the model
to end with a JSON object, which is graded against the truth: the planted
marker's yaw (synth::moving_marker_lon), and the check and analyze
tools' own results for the file (so a model is graded on reporting what
the tools found, not on the tools). Transcripts go to OUT/<model>/<task>
.jsonl and the verdicts to OUT/summary.json.

Only the standard library. Media: `cargo xtask gen-media --out DIR`.

    local_agents.py --spherecut target/debug/spherecut --media DIR \\
        --skill crates/agent/skill/spherecut/SKILL.md \\
        --base http://huginn.internal:8080 --models m1,m2 --out DIR
"""

import argparse
import base64
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

# A 1x1 red PNG, to ask whether a model takes images.
RED_PNG = (
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC"
)


class Mcp:
    """`spherecut mcp` on stdio: newline-delimited JSON-RPC."""

    def __init__(self, exe, root):
        self.p = subprocess.Popen(
            [exe, "mcp", "--read-root", root],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        self.n = 0
        init = self.rpc(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "local-agents", "version": "1"},
            },
        )
        self.instructions = init.get("instructions", "")
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        self.tools = self.rpc("tools/list", {})["tools"]

    def send(self, msg):
        self.p.stdin.write(json.dumps(msg) + "\n")
        self.p.stdin.flush()

    def rpc(self, method, params):
        self.n += 1
        self.send({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params})
        while True:
            line = self.p.stdout.readline()
            if not line:
                raise RuntimeError("spherecut mcp ended")
            msg = json.loads(line)
            if msg.get("id") == self.n:
                if "error" in msg:
                    raise RuntimeError(json.dumps(msg["error"]))
                return msg["result"]

    def call(self, name, args):
        return self.rpc("tools/call", {"name": name, "arguments": args})

    def close(self):
        self.p.stdin.close()
        self.p.wait(timeout=30)


def chat(base, model, messages, tools=None, timeout=900):
    body = {"model": model, "messages": messages, "temperature": 0.2}
    if tools:
        body["tools"] = tools
    req = urllib.request.Request(
        base.rstrip("/") + "/v1/chat/completions",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json", "User-Agent": "spherecut-research"},
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.load(r)


def takes_images(base, model):
    try:
        chat(
            base,
            model,
            [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "What colour is this image? One word."},
                        {"type": "image_url", "image_url": {"url": "data:image/png;base64," + RED_PNG}},
                    ],
                }
            ],
            timeout=600,
        )
        return True
    except urllib.error.HTTPError:
        return False


def tasks(media, mcp):
    """Each task: its prompt, the answer's shape, and a grader."""
    out = []

    # Recipe 1: the planted marker, 3 degrees a frame from yaw 0.
    clip = "grid-hevc8-30.mp4"

    def marker_yaw(seconds):
        frame = round(seconds * 30)
        return ((frame * 3.0 + 180.0) % 360.0) - 180.0

    def grade_marker(ans):
        ss = ans.get("sightings") or []
        if len(ss) < 3:
            return False, f"{len(ss)} sightings, 3 asked for"
        offs = []
        for s in ss:
            d = (float(s["yaw"]) - marker_yaw(float(s["seconds"])) + 180) % 360 - 180
            offs.append(round(d, 1))
        ok = all(abs(d) <= 10 for d in offs) and all(abs(float(s.get("pitch", 0))) <= 10 for s in ss)
        return ok, f"yaw off by {offs}"

    out.append(
        (
            "marker",
            f"In {clip}, a small marker moves along the horizon (the equator of the 360 picture). "
            "Find its yaw and pitch at the start, the middle and the end of the clip.",
            '{"sightings": [{"seconds": <number>, "yaw": <degrees>, "pitch": <degrees>}, ...]}',
            grade_marker,
        )
    )

    # Recipe 2: an export with no spherical metadata, for YouTube.
    for name in ["grid-hevc8-30-untagged.mp4", "grid-hevc8-30.mp4"]:
        truth = json.loads(mcp.call("check", {"file": name, "target": "youtube_360"})["content"][0]["text"])
        failed = sorted(r["id"] for r in truth["rules"] if not r["ok"])

        def grade_check(ans, truth=truth, failed=failed):
            got = sorted(set(ans.get("failed_rules") or []))
            ok = ans.get("pass") == truth["pass"] and got == failed
            return ok, f"pass {ans.get('pass')} (truth {truth['pass']}), rules {got} (truth {failed})"

        out.append(
            (
                "check-" + name.removesuffix(".mp4"),
                f"Is {name} ready to upload to YouTube as a 360 video? If not, say what fails and how to fix it.",
                '{"pass": <true|false>, "failed_rules": [<the failing rules\' ids>]}',
                grade_check,
            )
        )

    # Recipe 3: planted gaps, duplicate timestamps and repeated pictures.
    jd = "jitter-dups.mp4"
    s = json.loads(mcp.call("analyze", {"file": jd, "kind": "timing"})["content"][0]["text"])["summary"]
    truth = {k: s[k]["count"] for k in ["gaps", "dups", "repeated_pictures"]}

    def grade_timing(ans, truth=truth):
        got = {k: ans.get(k) for k in truth}
        return got == truth, f"{got} (truth {truth})"

    out.append(
        (
            "stutter",
            f"Someone says {jd} stutters. Find out why, with counts.",
            '{"gaps": <count>, "dups": <count of duplicate timestamps>, "repeated_pictures": <count>}',
            grade_timing,
        )
    )
    return out


def last_json(text):
    """The last top-level {...} in `text` that parses, not an object
    nested in it."""
    text, i, last = text or "", 0, None
    dec = json.JSONDecoder()
    while (i := text.find("{", i)) >= 0:
        try:
            last, i = dec.raw_decode(text, i)
        except json.JSONDecodeError:
            i += 1
    return last


def run(args, model, vision, task, mcp, skill, log):
    name, prompt, shape, grade = task
    tools = [
        {"type": "function", "function": {"name": t["name"], "description": t.get("description", ""), "parameters": t["inputSchema"]}}
        for t in mcp.tools
    ]
    system = (
        "You are an agent with Spherecut's tools for 360° video. The files are under the server's read root; "
        "name them by their relative paths.\n\n# The server's instructions\n\n"
        + mcp.instructions
        + "\n\n# The skill\n\n"
        + skill
        + ("" if vision else "\n\nYou can't see images: an image a tool returns is left out, so rely on the JSON.")
    )
    messages = [
        {"role": "system", "content": system},
        {"role": "user", "content": f"{prompt}\n\nWhen you are done, end your answer with one JSON object: {shape}"},
    ]
    stats = {"steps": 0, "calls": 0, "errors": 0, "images": 0, "prompt_tokens": 0, "completion_tokens": 0}
    t0 = time.time()
    answer = None
    for _ in range(args.max_steps):
        stats["steps"] += 1
        try:
            r = chat(args.base, model, messages, tools)
        except (urllib.error.URLError, TimeoutError) as e:
            log.write(json.dumps({"error": str(e)}) + "\n")
            break
        u = r.get("usage") or {}
        stats["prompt_tokens"] = max(stats["prompt_tokens"], u.get("prompt_tokens", 0))
        stats["completion_tokens"] += u.get("completion_tokens", 0)
        msg = r["choices"][0]["message"]
        log.write(json.dumps({"assistant": msg}) + "\n")
        calls = msg.get("tool_calls") or []
        messages.append({k: v for k, v in msg.items() if k in ("role", "content", "tool_calls")})
        if not calls:
            answer = msg.get("content") or ""
            break
        pictures = []
        for c in calls:
            stats["calls"] += 1
            fn = c["function"]
            try:
                a = json.loads(fn.get("arguments") or "{}")
                res = mcp.call(fn["name"], a)
            except (json.JSONDecodeError, RuntimeError) as e:
                res = {"isError": True, "content": [{"type": "text", "text": f"not called: {e}"}]}
            if res.get("isError"):
                stats["errors"] += 1
            parts = []
            for item in res.get("content", []):
                if item["type"] == "text":
                    parts.append(item["text"])
                elif item["type"] == "image":
                    stats["images"] += 1
                    if vision:
                        pictures.append(item)
                        parts.append(f"[image {len(pictures)}: attached in the next message]")
                    else:
                        parts.append("[image left out: this model takes no images]")
            text = "\n".join(parts)
            log.write(json.dumps({"tool": fn["name"], "args": fn.get("arguments"), "error": bool(res.get("isError")), "result": text[:4000]}) + "\n")
            messages.append({"role": "tool", "tool_call_id": c.get("id", ""), "content": text})
        if pictures:
            messages.append(
                {
                    "role": "user",
                    "content": [{"type": "text", "text": "The images the tools returned, in order:"}]
                    + [{"type": "image_url", "image_url": {"url": f"data:{p['mimeType']};base64,{p['data']}"}} for p in pictures],
                }
            )
    stats["seconds"] = round(time.time() - t0, 1)
    ans = last_json(answer) if answer is not None else None
    if ans is None:
        ok, why = False, "no answer" if answer is None else "no JSON in the answer"
    else:
        try:
            ok, why = grade(ans)
        except (KeyError, TypeError, ValueError) as e:
            ok, why = False, f"answer malformed: {e}"
    log.write(json.dumps({"answer": answer, "parsed": ans, "ok": ok, "why": why, "stats": stats}) + "\n")
    return {"model": model, "task": name, "vision": vision, "ok": ok, "why": why, **stats}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--spherecut", required=True)
    ap.add_argument("--media", required=True)
    ap.add_argument("--skill", required=True)
    ap.add_argument("--base", required=True)
    ap.add_argument("--models", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--max-steps", type=int, default=16)
    ap.add_argument("--only", default="")
    args = ap.parse_args()
    skill = open(args.skill).read().split("---", 2)[-1]
    os.makedirs(args.out, exist_ok=True)
    results = []
    for model in [m for m in args.models.split(",") if m]:
        vision = takes_images(args.base, model)
        mcp = Mcp(args.spherecut, args.media)
        try:
            for task in tasks(args.media, mcp):
                if args.only and task[0] not in args.only.split(","):
                    continue
                d = os.path.join(args.out, model.replace("/", "_"))
                os.makedirs(d, exist_ok=True)
                with open(os.path.join(d, task[0] + ".jsonl"), "w") as log:
                    res = run(args, model, vision, task, mcp, skill, log)
                print(json.dumps(res), flush=True)
                results.append(res)
                with open(os.path.join(args.out, "summary.json"), "w") as f:
                    json.dump(results, f, indent=1)
        finally:
            mcp.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
