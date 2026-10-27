# SolidCraft iPad AI: test prompts

Copy and paste these into the AI window, then rate the result: **Good**, **Partly** or **Poor**.
Then say "sync ai" in Claude Code to review the log.

Status: **Worked** = rated Good in testing. **Seen** = ran, with notes. **Untried** = suggested, not run yet.

## Build the test plate

**Worked.** Starts a new design (in a new tab), then builds the whole plate. Volume should be about 35,924 mm³.

```
Start a new design, then build a mounting plate. A base plate 80 mm long, 50 mm wide and 8 mm thick, with its corner at the origin and all four vertical edges filleted 6 mm. Two 8 mm diameter through-holes on the plate's centre line, 15 mm in from each short end. A round boss 24 mm in diameter and 15 mm tall, centred on the plate and joined to its top face, with a 10 mm diameter hole through the boss and the plate. Check the result with inspect_design and tell me the final volume.
```

## Build the plate and split off the boss

**Worked.** The same plate, then the boss is split off, renamed and coloured. Goes to Claude.

```
Start a new design, then build a mounting plate. A base plate 80 mm long, 50 mm wide and 8 mm thick, with its corner at the origin and all four vertical edges filleted 6 mm. Two 8 mm diameter through-holes on the plate's centre line, 15 mm in from each short end. A round boss 24 mm in diameter and 15 mm tall, centred on the plate and joined to its top face, with a 10 mm diameter hole through the boss and the plate. Check the result with inspect_design and tell me the final volume. Then split the boss off the plate, name it Boss and make it red.
```

Or as a second request after the plate exists:

```
Split the boss off the plate, name it Boss and make it red.
```

## Editing an existing model

**Worked.** Needs the sample plate with its Holes sketch (not the plate built by the prompt above, which has no sketches).

```
Edit holes sketch - change the diameters to 5mm
```

**Worked.**

```
Move the plate 30 mm to the left
```

## Show and hide

**Worked.** Replays at once once it has been rated Good.

```
Hide all
```

```
Show all
```

**Untried.** Rules like those in the ShowHide add-in.

```
Hide everything except the plate
```

## Documents

**Seen.** Opens a new tab; the old design stays open. Say it clearly, or type it. "Great to do design" is a known mishearing.

```
Create new design
```

## Simple shapes and edges

**Seen.** The Apple on-device model handles these. Check that the cube is a cube.

```
Create a 30 mm cube
```

```
Fillet all edges of the cube 5 mm
```

```
Chamfer the top edges 2 mm
```

## Things that don't exist (a useful negative test)

**Seen.** SolidCraft has no body-delete command. A good answer says so instead of looping.

```
Delete body
```

## Comparison test: duplicate the plate, side by side, with a filleted boss

The same job, tried with every AI mode, so you can compare. It is what the Claude-on-the-Mac (Pro plan) test did, including the clarifying answers and the reframing, written as one prompt.

**Starting point (the same every time):** the app's sample plate only, one body named Plate, 80 × 50 mm, boss on top (z 0 to 22). Quit and reopen the app, or start from the sample, so every mode starts identically. The AI window must be open.

**The prompt**

```
Duplicate the plate and place the copy beside the original along the long edge: the two 80 mm long edges face each other with a 10 mm gap, so the copy covers y 60 to 110 and the same x and z as the original. Then add a 5 mm fillet around the base of the boss on the copy only, where the boss meets the plate (not the top rim). Leave the original plate unchanged. Then fit the view so both plates are visible.
```

**A short version, for models that do better with small steps** (send one at a time):

```
1. Copy the plate and move the copy 60 mm in Y.
2. Fillet the base of the boss on the copy, 5 mm.
3. Fit the view.
```

**What a correct result looks like** (check with Inspect, or the log after "sync ai"):

| | Plate | Plate (2) (the copy) |
|---|---|---|
| Position | x 0 to 80, y 0 to 50, z 0 to 22 | x 0 to 80, y 60 to 110, z 0 to 22 |
| Volume | 34,793.8 mm³ (unchanged) | 35,236.2 mm³ (+442.4 from the fillet) |
| Boss base | sharp corner | smooth 5 mm blend; the top rim stays sharp |

Common mistakes to look for: the copy moved along X instead of Y, no gap or the wrong gap, the fillet on the original or on both plates, the fillet on the top rim of the boss, a fillet on every edge, the copy made by rebuilding a plate from scratch, the original moved instead of copied.

**How to run the comparison**

| Mode | How to start it | What to expect |
|---|---|---|
| Apple on-device | Ask | No copy or boss-edge tool: expect a hand-over (its changes are undone first) |
| Claude Haiku | Ask Claude | Should find `solid.move` with `copy` and `solid.fillet`; may take several calls and picking the right boss edge is the hard part |
| Qwen3 4B (offline) | Ask Qwen3 4B | Its tools cannot copy a body or pick the boss base: expect a polite "can't do that" |
| Claude Sonnet or Opus on the Mac | Ask Claude (Pro), then "pick up the latest iPad request" | The tested path: two commands, verified on the Mac before the iPad runs them |

Rate each Good, Partly or Poor, then say "sync ai", and I will put the calls, the results and the volumes side by side.

## Tips

- If the Apple model creates stray shapes, say "undo" or use New chat.
- Keywords such as hole, boss, sketch, split, colour and red send a request straight to Claude.
- Speak: tap Speak, talk, then pause. The request sends itself after about 1.5 seconds.
- A request rated Good is remembered and replays next time without any model call.
