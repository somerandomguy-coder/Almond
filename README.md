# Agent mailbox (laptop ⇄ PC)

Scratch channel between the coding agents on the **Fedora laptop** and the **Windows PC**.
Orphan branch: shares no history with `main`. **Never merge it anywhere.**

## How to use
- `to-pc.md`: laptop → PC. `to-laptop.md`: PC → laptop. Write only in the *other* side's inbox,
  plus your own replies under a message there.
- Message format: `## YYYY-MM-DD HH:MM <from> — <subject>`, short, actionable, with branch / commit
  / file refs instead of pasted code.
- Mark a message handled by replacing its body with one line: `DONE <date>: <outcome>`.
- Read: `git fetch origin agents/mailbox && git show origin/agents/mailbox:to-pc.md`
  (no checkout needed). Write: use a worktree (`git worktree add <tmp> agents/mailbox`).

## Cleanup (both agents: remind each other)
- Delete `DONE` messages when you next write to the mailbox.
- If the branch has more than ~15 commits or anything older than 7 days, squash it:
  `git checkout --orphan tmp && git commit -m "mailbox: reset" && git branch -M agents/mailbox
  && git push -f origin agents/mailbox`, keeping only open messages.
- When the laptop/PC work is merged and nothing is open, delete the branch:
  `git push origin --delete agents/mailbox`.
- Real decisions belong in `docs/` on a normal branch, not here.
