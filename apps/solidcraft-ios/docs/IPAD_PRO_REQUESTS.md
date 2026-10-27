# Answering "Ask Claude (Pro)" requests from the iPad

For a Claude Code session on the Mac. The user says something like **"pick up the latest iPad request"** or **"check the iPad results"**. Nothing here runs by itself: the user starts each step, so the work stays inside a normal interactive Claude Code session.

The iPad and the Mac must be on the same network, with the iPad awake. Use `dangerouslyDisableSandbox` for `xcrun devicectl`.

```
D=<device UDID>                      # xcrun devicectl list devices
APP=<your bundle id>
CLI=<repo>/target/release/solidcraft-cli
WORK=<the session's scratchpad>/ipadpro     # a new folder per request: $WORK/<id>/
```

If `$CLI` is missing, build it: `CARGO_TARGET_DIR=$PWD/target cargo build --release -p solidcraft-cli` (about 3 minutes).

## Step B: pick up a request and answer it

1. **Find the newest request** (folder names are millisecond timestamps; a request is complete when `ready.txt` is in it):
   ```
   xcrun devicectl device info files --device $D --domain-type appDataContainer --domain-identifier $APP --subdirectory Documents/ai/requests
   ```
2. **Pull its files**, one by one (`<id>` is the folder name):
   ```
   for f in request.json inspect.txt design.json screen.png ready.txt; do
     xcrun devicectl device copy from --device $D --domain-type appDataContainer --domain-identifier $APP \
       --source Documents/ai/requests/<id>/$f --destination $WORK/<id>/$f
   done
   ```
3. **Read them.** `request.json` has the prompt. `inspect.txt` lists the bodies with their extents `[x min..max, y min..max, z min..max]`, the timeline features, the parameters, and (if there is one) the user's selection with a point on each face. `screen.png` is what the iPad showed (open it with Read). `design.json` is the full design.
4. **Ask the user** anything unclear before writing a script (they are in the remote-control session).
5. **Write the script:** a JSON array of engine commands, `[{"command": "solid.box", "params": {...}}, ...]`. Find a command's parameters with `$CLI commands`, or the iPad's own `find_commands`. Rules of thumb:
   - Use the body and feature names in `inspect.txt` exactly.
   - A join that only touches a face fails ("cannot preserve the original faces"): overlap the profile by 1 mm.
   - The iPad stops at the first error, so keep steps small and independent, and put risky ones last.
   - Do not use `file.new` or anything that closes the design unless that is what was asked.
6. **Test on the Mac against the iPad's design:**
   ```
   $CLI run script.json --in $WORK/<id>/design.json --results --save $WORK/<id>/after.solidcraft
   $CLI snapshot script.json --in $WORK/<id>/design.json --out $WORK/<id>/shot.png --view iso
   ```
   Open `shot.png` with Read and check it looks right. Check the extents with `$CLI eval $WORK/<id>/after.solidcraft` if the request was about sizes or positions.
7. **Write `response.json`** and push it:
   ```
   {"id": <id>, "title": "short title", "explanation": "what the steps do, in a sentence or two", "script": [ ...the commands... ]}
   ```
   ```
   xcrun devicectl device copy to --device $D --domain-type appDataContainer --domain-identifier $APP \
     --source $WORK/<id>/response.json --destination Documents/ai/responses/<id>/response.json
   ```
8. Tell the user: the iPad shows the steps in the AI window; tap **Run these steps** (or tick "Run its steps as soon as they arrive").

## Step D: check the results

After the user has run it on the iPad and says "check the iPad results":

```
for f in results.json after.json after.png results-ready.txt; do
  xcrun devicectl device copy from --device $D --domain-type appDataContainer --domain-identifier $APP \
    --source Documents/ai/requests/<id>/$f --destination $WORK/<id>/$f
done
```

- `results.json`: `ok`, and each step's `ok` / `result` / `error`. A failed step stops the run.
- `after.png`: the iPad after the run (Read it). `after.json`: the design afterwards (run `$CLI eval` or `--in` it).
- If something went wrong or does not match, write a corrected `response.json` for the **same id** (or a new request from the user) and go to step 7 again. The iPad offers "Undo these steps" after a run.

## Starting from the Mac

The user can also ask for something directly in this session, with no request from the iPad. Write the script the same way, test it against the latest design you have (`design.json` or `after.json` from an earlier bundle; the iPad may have been restarted since, so check what state it is in), then push `response.json` with a **new id** (a millisecond timestamp, e.g. `python3 -c "import time;print(int(time.time()*1000))"`) to `Documents/ai/responses/<new id>/response.json`. The app checks for replies every few seconds while its AI window is open and offers the steps with **Run these steps**; it creates the request folder for the results itself. Pull `results.json`, `after.json` and `after.png` from `Documents/ai/requests/<new id>/` afterwards, as in step D. Without a request from the iPad there is no `screen.png`, so ask the user to tap Ask Claude (Pro) if you need to see the screen.

## Also useful for the local-model work

A request bundle is an exact record of a design and a prompt. To see why the local (Qwen) or Apple model got something wrong, build the same context from `inspect.txt` and run it through `scratchpad/llmtest/bench2.py` style tests.

## What the iPad writes (for reference)

```
Documents/ai/requests/<id>/   request.json  inspect.txt  design.json  screen.png  ready.txt
                              results.json  after.json  after.png  results-ready.txt   (after Run)
Documents/ai/responses/<id>/  response.json   (written by the Mac)
```
