# Board of Directors (bod)

Board of Directors (`bod`) is a multi-agent code-review CLI that runs parallel AI reviewers, consolidates feedback, and can assist with automated fixes.

## Requirements

- Copilot CLI installed and available in the environment

## Commands

`bod review`
Run parallel reviews for the current branch.

`bod review consolidate`
Consolidate the latest review round for the current branch into a single report.

`bod consolidate`
Consolidate review findings (non-branch-specific).

`bod bugfix --timeout <seconds> --severity <critical|high|medium|low>`
Run the autonomous review-fix loop. Example: `bod bugfix --timeout 3600 --severity high`

`bod init [--global] [--reconfigure]`
Interactive model/config setup. `--global` writes to `~/.config/board-of-directors/.bodrc.toml`

`bod version`
Print version information.

## State location

Runtime and configuration are stored outside the repository to keep the working tree clean:

```bash
$HOME/.config/board-of-directors/<repo-scope>/
```

Replaces `<repo-scope>` with a sanitized form of the repo directory name. 

