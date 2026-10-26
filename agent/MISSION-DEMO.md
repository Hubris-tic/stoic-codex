# A whole mission, verbatim

The transcript below is a real run of the shipped `stoic-codex` binary against a real git
repository over a real loopback socket. The only thing that is not a real model is the words
coming back: `cli/dev/stub_mission.py` speaks the same chat-completions shape and plays
Strategos and the operators of the Phalanx, so the prompts are the orchestrator's own and every
other step — naming, worktrees, sandboxed tool calls, commits, the review, the apply step — is
the real code.

Reproduce it:

```bash
python3 cli/dev/stub_mission.py --port 8099 &

mkdir -p /tmp/demo-repo && cd /tmp/demo-repo && git init -q -b main
printf 'demo repo\n' > README.md && git add -A
git -c user.name=Demo -c user.email=demo@localhost commit -qm initial

export STOIC_CODEX_HOME=/tmp/demo-home STOIC_CODEX_STUB_KEY=not-a-key
mkdir -p $STOIC_CODEX_HOME/.stoic-codex
cat > $STOIC_CODEX_HOME/.stoic-codex/config.json <<'JSON'
{ "memory": { "backend": "file" },
  "providers": { "default": "openai-chat", "model": "stub",
                 "base_url": "http://127.0.0.1:8099", "api_key_env": "STOIC_CODEX_STUB_KEY" } }
JSON

stoic-codex mission run "add one file per code" --repo /tmp/demo-repo --agents 3 --parallel 3
```

## What Strategos would do (stdout), before anything is deployed

`mission plan` asks the model once and names the Phalanx, but touches no file:

```
[strategos] plan    source=strategos
[strategos] name    slot-1     → x0-core · the base context harvester
[strategos] name    slot-2     → x1-write · the primary logic generator
[strategos] name    slot-3     → x2-check · the testing and validation engine

[x0-core]   task    create x0-core.txt containing the word core
[x1-write]  task    create x1-write.txt containing the word write
[x2-check]  task    create x2-check.txt containing the word check
```

## Progress (stderr)

Three operators, three worktrees, genuinely concurrent — note the interleaving, and note that
the log is a fixed-column table: `[code]`, then a verb, then `key=value`. Nothing is a sentence
you have to read to find the actor.

```
[strategos] plan    reading the layout
[strategos] name    slot-1     → x0-core · the base context harvester
[strategos] name    slot-2     → x1-write · the primary logic generator
[strategos] name    slot-3     → x2-check · the testing and validation engine
[strategos] planned source=strategos assignments=3
[x0-core]   task    create x0-core.txt containing the word core
[x1-write]  task    create x1-write.txt containing the word write
[x2-check]  task    create x2-check.txt containing the word check
[strategos] deploy  x0-core      branch=stoic-codex/x0-core
[strategos] deploy  x1-write     branch=stoic-codex/x1-write
[strategos] deploy  x2-check     branch=stoic-codex/x2-check
[x2-check]  tool    write_file       {"content":"x2-check\n","path":"x2-check.txt"}
[x2-check]  result  write_file       created x2-check.txt (9 bytes) (0ms)
[x0-core]   tool    write_file       {"content":"x0-core\n","path":"x0-core.txt"}
[x0-core]   result  write_file       created x0-core.txt (8 bytes) (0ms)
[x1-write]  tool    write_file       {"content":"x1-write\n","path":"x1-write.txt"}
[x1-write]  result  write_file       created x1-write.txt (9 bytes) (0ms)
[x1-write]  return  state=done
[x0-core]   return  state=done
[x2-check]  return  state=done
[strategos] review  reports=3
[x0-core]   verdict accepted   wrote exactly the file it was given
[x1-write]  verdict accepted   wrote exactly the file it was given
[x2-check]  verdict accepted   wrote exactly the file it was given
[strategos] apply   x0-core.txt ← x0-core
[strategos] apply   x1-write.txt ← x1-write
[strategos] apply   x2-check.txt ← x2-check
[strategos] done    applied=3 conflicts=0
```

## Report (stdout), exit code 0

```
CODE         STATE       VERDICT    FILES    TOKENS TASK
x0-core      done        accepted       1       120 create x0-core.txt containing the word core
x1-write     done        accepted       1       120 create x1-write.txt containing the word write
x2-check     done        accepted       1       120 create x2-check.txt containing the word check


Strategos · base main · 3 operator(s) of The Phalanx · 40 ms in parallel · plan strategos
Strategos reviewed 3 report(s)
applied  3 file(s)
  x0-core.txt  ← x0-core
  x1-write.txt  ← x1-write
  x2-check.txt  ← x2-check
commit   15008cd
```

## What the repository looks like afterwards

```
$ ls
README.md  x0-core.txt  x1-write.txt  x2-check.txt

$ git log --oneline
15008cd stoic-codex mission: 3 file(s) from 3 operators of The Phalanx under Strategos
0b58477 initial

$ git status --porcelain     # empty: nothing left staged, no worktrees lying around
$ git branch --list          # empty apart from main: branches whose work landed are gone
* main
```

One commit for the mission, not three. The worktrees live under
`.git/stoic-codex-missions/<code>` and are removed on the way out; the branches
(`stoic-codex/x0-core`, `stoic-codex/x1-write`, `stoic-codex/x2-check`) existed only while the agents
worked, because their files landed.

## The failure paths, also real

Same stub, started with `--lazy` so every operator answers in prose and touches nothing:

```
[strategos] review  reports=3
[x0-core]   verdict rejected   wrote exactly the file it was given (nothing usable to apply)
[x1-write]  verdict rejected   wrote exactly the file it was given (nothing usable to apply)
[x2-check]  verdict rejected   wrote exactly the file it was given (nothing usable to apply)
[strategos] done    applied=0 conflicts=0
error: no operator of The Phalanx produced an applicable change (0 failed, 3 rejected)
```

```
CODE         STATE       VERDICT    FILES    TOKENS TASK
x0-core      no changes  rejected       0       120 create x0-core.txt containing the word core
x1-write     no changes  rejected       0       120 create x1-write.txt containing the word write
x2-check     no changes  rejected       0       120 create x2-check.txt containing the word check


Strategos · base main · 3 operator(s) of The Phalanx · 44 ms in parallel · plan strategos
Strategos reviewed 3 report(s)
applied  nothing
$ echo $?
1
```

Nothing is committed and the working tree is untouched — an empty mission is a failure even
when every model answered politely.

## A bug this demo found

The first rehearsal ran in a repository with **no `user.name` / `user.email`** configured —
exactly what a fresh clone on a fresh machine looks like. Every operator wrote its file, and
then `git commit` inside the worktree refused, so all three came back `failed` with their work
stranded. The fix is `worktree::identity_flags`: the orchestrator commits as
`Stoic Codex <stoic-codex@localhost>` **only when no identity is configured anywhere**, and the CLI's
mission tests now run in a repo with no identity at all so the path stays covered.

## Why the naming is in the log

`slot-1 → x0-core · the base context harvester` is one line because of the question the log has
to answer at a glance: *which one is that?* An operator's name is not a mnemonic, it is a claim
about the work it was given, and the line prints both halves — the slot it replaced and the role
the code stands for. If the code ever stops matching the assignment, the plan drifted, and the
log is where that is visible first.
