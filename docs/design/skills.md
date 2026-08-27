# Skills — one on-disk source, the Pi loop, and external agents

> Research pass, 2026-08-27, branch `agent-code-execution`. Verified against the
> shipped source of `@earendil-works/pi-agent-core@0.80.2` in `node_modules/`,
> the pi monorepo at `earendil-works/pi@1defa151` (v0.84.3 — `badlogic/pi-mono`
> now redirects there), the Luma tree, and the published Agent Skills spec.
> Nothing here is implemented yet.

Skills are the lighting-craft playbooks the track copilot reads before it authors:
genre technique (`four-on-the-floor`, `heavy-bass`), craft (`color`,
`contrast-and-darkness`, `rig-craft`), and method (`finding-things-in-audio`). Ten
of them exist today, bundled into the webview bundle and reachable only from the
TypeScript loop. The Rust loop cannot see them — while telling its model to use
them — and no external agent can either.

This document settles where skills live, what format they are in, how the Pi-based
in-app loop consumes them, and how `luma-mcp` exposes them to Claude Code and Codex.

---

## 1. Decision in one paragraph

Skills become **plain Agent Skills directories on disk** at
`resources/skills/<name>/SKILL.md`, bundled the way fixtures already are
(`tauri.conf.json` `resources`), with `SKILL.md` frontmatter restricted to the
spec's `name` + `description`. **Rust owns the one registry** — a small
`agent::skills` module that discovers, validates and holds them — and it is the
single source for three consumers: the in-app loop (which mirrors Pi exactly:
an `<available_skills>` listing in the system prompt plus a `skill` tool that
returns the body), the TypeScript loop (same listing, same tool, over a Tauri
command), and `luma-mcp` (a `skill` tool alongside `open`/`python`, plus the same
listing folded into `open`'s reply, plus MCP `prompts/*` so Claude Code users get
`/mcp__luma__<skill>` for free). The webview stops being the place skills live;
`import.meta.glob` and `BUNDLED_SKILL_SOURCES` are deleted.

---

## 2. What exists today

### 2.1 The TypeScript implementation

| File | Role |
|---|---|
| `src/shared/lib/agent/skills/<name>/SKILL.md` | 10 skills, 2.6–4.3 KB each, ~33 KB total |
| `src/shared/lib/agent/skills/bundled-skills.ts` | `import.meta.glob("./*/SKILL.md", { query: "?raw", eager: true })` → `BUNDLED_SKILL_SOURCES` |
| `src/shared/lib/agent/skills/skill-loader.ts` | `Skill` type, `SkillLoader`, `skillToolDescription`, `buildSkillTool` |
| `src/shared/lib/agent/skills/index.ts` | facade + `skillToolLabel` chat-row label |
| `src/shared/lib/agent/skills/skill-loader.test.ts` | 11 vitest cases |
| `src/shared/lib/agent/frontmatter.ts` | shared `key: value` frontmatter parser (also used by `subagents/agent-loader.ts`) |

Wiring: `src/features/track-editor/agent/track-agent.ts:88` and `:138` put
`skill: buildSkillTool()` into both the parent and the subagent tool sets;
`VOCAB` at `:38-46` gives it the chat verb "Reading / Read a skill".

The mechanism is a **tool whose description enumerates every skill**
(`skill-loader.ts:76-88`):

```
Load a scoring playbook — genre-specific technique written by lighting designers…

Available skills:
- color: Palettes, temperature, and what hues mean. …
- heavy-bass: Dubstep, riddim, tearout, hard trap — punch music. …
…
```

`execute({name})` returns `{name, body}` and `toModelOutput` projects it to the
raw markdown body. Unknown names throw with the valid list appended — a good
"errors out of existence" touch.

**It works, and the shape is basically right.** The problems are all about *where
it lives* and *who can reach it*.

### 2.2 The Rust loop

`src-tauri/src/agent/` has no notion of skills at all (`grep -rni skill
--include='*.rs'` → zero hits), and `tools::registry` returns exactly one tool:

```rust
// src-tauri/src/agent/tools/mod.rs:129-140
pub fn registry(kind: super::AgentKind) -> ToolRegistry {
    match kind {
        AgentKind::TrackCopilot | AgentKind::PatternGraph => {
            ToolRegistry::new(vec![Arc::new(python::PythonTool)])
        }
    }
}
```

Yet its system prompt tells the model to use a tool that does not exist —
`src-tauri/src/agent/prompts/track.md:25`:

> Then read the skill(s) that fit — the `skill` tool carries genre technique,
> craft, and analysis playbooks.

**Smell #1 (tier 1).** The Rust loop instructs a model to call a tool it was never
given. Whatever else this design settles, that line and that registry must agree.

### 2.3 The prompt duplication under it

`src-tauri/src/agent/prompts/track.md` and
`src/features/track-editor/agent/build-context.ts` are the **same prose, twice** —
byte-identical from "## One working surface" through the end of "## Voice", with
the TS copy adding only a `## Current track` header block and template
interpolation.

**Smell #2 (tier 1).** Two copies of the same system prompt, in two languages, one
of which is `include_str!`-shared already for a sibling file. The precedent for
fixing it is right there — `python-tool.md` is read by *both* sides from one file:

```ts
// src/shared/lib/agent/python-tool.ts:9-11
// The one copy of the tool description, shared with the Rust agent loop,
// which reads the same file through `include_str!`.
import DESCRIPTION from "../../../../src-tauri/src/agent/prompts/python-tool.md?raw";
```

```rust
// src-tauri/src/agent/tools/python.rs:24-29
/// Public because it is the contract for *any* host that exposes this kernel —
/// `luma-mcp` hands the same text to an out-of-process coding agent, and a
/// second wording would be a second tool.
pub const PYTHON_TOOL_DESCRIPTION: &str = include_str!("../prompts/python-tool.md");
```

That comment is the whole thesis of this document, generalised: *a second wording
would be a second tool*. Skills are the same class of artifact.

### 2.4 The MCP surface

`src-tauri/src/bin/luma-mcp.rs` exposes four tools — `open`, `python`, `reset`,
`cancel` — over the hand-rolled `src-tauri/crates/mcp-stdio` crate (207 lines, one
dependency: `serde_json`). Its `initialize` declares `"capabilities": {"tools":{}}`
and `route()` answers exactly `initialize` / `ping` / `tools/list` / `tools/call`;
everything else is `-32601 unknown method`, pinned by a test that specifically
asserts this for `resources/list` (`crates/mcp-stdio/src/lib.rs:190-197`).

An external agent connected to `luma-mcp` therefore *is* the track copilot — with
the real kernel and the real bindings — but with **none of the craft**. It has the
python tool description and whatever its own harness told it, and no access to the
ten playbooks that make Luma's in-app agent good at this. That gap is the reason
this document exists.

---

## 3. How Pi does it

Pi (Mario Zechner; `badlogic/pi-mono` → `earendil-works/pi`) has **two skill
subsystems**, and the distinction matters because we depend on the wrong one.

| | live | future |
|---|---|---|
| Source | `packages/coding-agent/src/core/skills.ts` | `packages/agent/src/harness/skills.ts` |
| Shipped as | the `pi` CLI | `@earendil-works/pi-agent-core` — **what Luma depends on** |
| Style | sync, node `fs`, `ResourceDiagnostic` | async, abstract `ExecutionEnv`, `SkillDiagnostic` |
| Status at 0.84.3 | in production | `AgentHarness.skill()` is `this.unavailable("skill")` — throws `HarnessNotImplemented`; `create-harness.ts` wires no skills |

At the **0.80.2** we have in `node_modules`, the harness copy was still real —
`agent-harness.js:541-551` genuinely executes a turn with `formatSkillInvocation`.
It was stubbed out on the way to 0.84.3. Either way the two implementations agree
on format and differ only in plumbing, so "mirror Pi" is unambiguous; just don't
build on `AgentHarness.skill()`.

Below, §3.1–3.2 are read from `pi-agent-core@0.80.2` in `node_modules`; §3.3–3.4
are cross-checked against the live CLI at `1defa151`.

### 3.1 Discovery

`loadSkills(env, dirs)` walks each directory recursively over an abstract
`ExecutionEnv` (a `FileSystem` + `Shell`; `harness/types.d.ts:151-228`), and:

- in any directory, if a `SKILL.md` exists, **that is the skill and recursion into
  that directory stops** (`skills.js`, the `for (const entry of entries)` loop that
  `return`s immediately after loading);
- at the **root** directory only, loose `*.md` files also load as skills;
- `.gitignore` / `.ignore` / `.fdignore` are honoured, with patterns re-prefixed to
  the walk root;
- `.`-prefixed entries and `node_modules` are skipped;
- symlinks are resolved via `canonicalPath` before classification;
- a missing input directory is silently skipped — not an error.

Nothing throws. Every failure becomes a `SkillDiagnostic { type:"warning", code,
message, path }` with codes `file_info_failed | list_failed | read_failed |
parse_failed | invalid_metadata`. **A malformed skill degrades the listing; it
never fails the agent.** That is "define errors out of existence" applied to a
content pipeline, and it is the single best idea in Pi's implementation.

**Discovery policy is not the loader's job.** The CLI calls
`loadSkills({ …, includeDefaults: false })`
(`packages/coding-agent/src/core/resource-loader.ts:677`) and hands it an explicit
path list assembled by a separate package manager
(`packages/coding-agent/src/core/package-manager.ts:2385-2500`). The roots it
resolves:

| Root | Scope | Gate |
|---|---|---|
| `~/.pi/agent/skills/` | user | always |
| `~/.agents/skills/` | user | always |
| `<cwd>/.pi/skills/` | project | only if the folder is *trusted* |
| `<dir>/.agents/skills/`, cwd upward to the git root | project | only if trusted |
| `skills/` in an installed pi package | package | — |
| `skills: [...]` in `settings.json`, and repeatable `--skill <path>` | explicit | — |

Two things to steal and one to note:

- **The seam.** Discovery policy (which roots, which are trusted, provenance,
  enable/disable) lives above; parsing and validation live below and only ever see
  a path list. That is the right decomposition — by knowledge, not by chronology —
  and it is what lets Luma add `$APPCONFIG/skills/` later without touching the
  parser.
- **The trust gate.** Project-local skills are not loaded until the user trusts the
  folder (cached in `~/.pi/agent/trust.json`); `README:412` is blunt that "skills
  can instruct the model to perform any action including running executables."
- **`.claude/skills` is *not* read.** One grep hit for `.claude` in the whole
  source, and it's a Bedrock model id. Pi's interop bet is on the harness-neutral
  `.agents/skills`; Claude Code and Codex directories are opt-in through settings,
  which the docs spell out:

  ```json
  { "skills": ["~/.claude/skills", "~/.codex/skills"] }
  ```

### 3.2 Format and validation

Frontmatter is real YAML (`yaml@2.9.0`), split on the first `\n---` after a
leading `---`, with CRLF normalised first. `Skill` is:

```ts
// harness/types.d.ts:28-39
export interface Skill {
    name: string;          // lookup + model-visible listing
    description: string;   // short "when to use this"
    content: string;       // full instructions (body)
    filePath: string;      // absolute path — model-visible location, and the
                           // base for resolving relative references
    disableModelInvocation?: boolean;
}
```

Validation (`skills.js`, `validateName` / `validateDescription`):

- `name` defaults to the **parent directory name** if frontmatter omits it, and a
  mismatch between the two is a warning;
- `name` must be `^[a-z0-9-]+$`, ≤ **64** chars, no leading/trailing hyphen, no
  `--`;
- `description` is **required** (a skill with an empty description is dropped
  entirely — the only hard drop) and ≤ **1024** chars;
- `disable-model-invocation: true` hides the skill from the model's listing while
  leaving it available to explicit application invocation.

These numbers are exactly the Agent Skills spec's (§4), and Pi's docs claim
conformance with **one deliberate deviation**
(`packages/coding-agent/docs/skills.md`):

> Pi implements the [Agent Skills standard](https://agentskills.io/specification),
> warning about most violations but remaining lenient. Pi allows skill names to
> differ from their parent directory even though the standard disallows it; that
> rule is suboptimal for shared skill directories used across multiple agent
> harnesses.

That is the correct call and we should copy it: warn, don't drop.

Pi documents `license` / `compatibility` / `metadata` / `allowed-tools` /
`disable-model-invocation` in its frontmatter table, but **`allowed-tools` is
documented and never implemented** — zero hits for it in the source. A
silent-failure stub in the reference implementation is a good reason not to accept
the field ourselves (§8.1).

### 3.3 Progressive disclosure — the system-prompt block

`formatSkillsForSystemPrompt(skills)` filters out `disableModelInvocation` and
emits (live CLI wording, `packages/coding-agent/src/core/skills.ts:355-380`,
injected at `system-prompt.ts:66` and `:163`):

```xml
The following skills provide specialized instructions for specific tasks.
Use the read tool to load a skill's file when the task matches its description.
When a skill file references a relative path, resolve it against the skill directory
(parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.

<available_skills>
  <skill>
    <name>color</name>
    <description>Palettes, temperature, and what hues mean. …</description>
    <location>/abs/path/to/skills/color/SKILL.md</location>
  </skill>
  …
</available_skills>
```

XML-escaped, one block, in the **system prompt** — not a tool description. The
comment above it cites the spec directly: *"Uses XML format per Agent Skills
standard. See: https://agentskills.io/integrate-skills"*.

### 3.4 Tool vs prompt injection — the load-bearing difference

**Pi has no `skill` tool.** Its entire built-in tool vocabulary is eight names —
`read | bash | powershell | edit | write | grep | find | ls`
(`packages/coding-agent/src/core/tools/index.ts:93-104`) — of which four ship by
default. The listing carries a `<location>` and says *use the read tool*; the model
then reads that absolute path. Level 3 of progressive disclosure — `references/`,
`scripts/`, `assets/` — falls out for free, because the model already has a file
reader and a resolution rule for relative paths.

Pi is candid that this is probabilistic (`docs/skills.md`):

> When a task matches, the agent uses `read` to load the full SKILL.md (models
> don't always do this; use prompting or `/skill:name` to force it)

That caveat is the strongest argument for our keeping a tool (§8.4): a named tool
call is a harder affordance than "please read this path."

The second path is **explicit, user-initiated** invocation: the slash command is
`/skill:<name>` (the name is part of the command, not an argument), expanded by
pure text prefix-matching in `agent-session.ts:1333-1348` into a *user message*,
not a tool result:

```
<skill name="color" location="/abs/…/color/SKILL.md">
References are relative to /abs/…/color.

<body>
</skill>
```

with any trailing arguments appended after a blank line. The library's
`formatSkillInvocation` produces byte-identical output.

**Prompt templates are a separate primitive**, not a skill variant: `.pi/prompts/*.md`
and `~/.pi/agent/prompts/*.md`, invoked as `/<filename>`, with `$1` / `$@` /
`$ARGUMENTS` substitution and frontmatter `description` + `argument-hint`. A
template is *what the user asked*; a skill is *how to do it*. A template is never
advertised to the model; a skill's description always is.

Both formatters, plus `loadSkills`, are exported from the package root
(`dist/index.d.ts:3,7,13,14`), so they are usable without adopting `AgentHarness`
— which matters, because `AgentHarness.skill()` throws at 0.84.3.

### 3.5 Pi's position on MCP

Worth recording because it is the opposite of what §6 concludes. Pi ships **no MCP
at all**, deliberately (`README:499`):

> **No MCP.** Build CLI tools with READMEs (see Skills), or build an extension that
> adds MCP support.

For Pi, skills *replace* MCP; distribution goes through npm/git "pi packages"
instead. That is a coherent position for a coding agent whose model already has
`bash` and `read`. It is not available to us: Luma's value is a sandboxed kernel
with live bindings, which is exactly the thing a README cannot be. We take Pi's
skill *format and disclosure model*, and reject its conclusion that MCP is
unnecessary — because our MCP server is the product, and the skills are cargo it
should carry.

### 3.6 What Luma actually uses of Pi today

`src/shared/lib/agent/pi-agent-loop.ts` constructs the **low-level `Agent`**, not
`AgentHarness`. So none of §3.3–3.4 is in play: our skills are a tool description,
Pi's are a system-prompt block plus a file read. **We diverged from Pi on the one
axis Pi considers architectural** — and we did so silently, by never adopting the
layer that owns skills.

Note also that we cannot simply reuse `loadSkills`: it needs an `ExecutionEnv`,
which is a full `FileSystem` + `Shell` (`harness/types.d.ts:151-228`) and cannot be
stubbed usefully in a webview. The pure formatters (`formatSkillsForSystemPrompt`,
`formatSkillInvocation`) *are* reusable — but they take a `Skill[]` we would have to
produce anyway, and §8 puts that registry in Rust, so we mirror their **output
format** rather than importing them. One vocabulary; no dependency on a subsystem
whose maintainer has half-migrated it.

---

## 4. The Agent Skills spec

Anthropic released Agent Skills as an open standard on 2025-12-18; governance sits
with the Agentic AI Foundation under the Linux Foundation, alongside MCP.
Spec: <https://agentskills.io/specification>. Reference validator:
<https://github.com/agentskills/agentskills> (`skills-ref validate ./my-skill`).

### 4.1 Frontmatter

| Field | Required | Constraint |
|---|---|---|
| `name` | yes | ≤ 64 chars, `[a-z0-9-]+`, no leading/trailing/consecutive hyphen, **must match the parent directory name** |
| `description` | yes | ≤ 1024 chars, non-empty; states *what* and *when*; front-load trigger words |
| `license` | no | license name or bundled file reference |
| `compatibility` | no | ≤ 500 chars; environment requirements |
| `metadata` | no | arbitrary string→string map for extensions |
| `allowed-tools` | no | **experimental**; space-separated pre-approved tools, e.g. `Bash(git:*) Read` |

`description` is the trigger. It is the only text a client is required to keep in
context for a skill that is not loaded, so it does the whole job of routing.

### 4.2 Layout and progressive disclosure

```
skill-name/
├── SKILL.md      # required: frontmatter + instructions
├── scripts/      # optional: executable code
├── references/   # optional: deeper docs the agent reads on demand
└── assets/       # optional: templates, data, images
```

Three levels: **metadata** (~100 tokens/skill, always in context) → **instructions**
(the `SKILL.md` body, on activation, recommended < 5000 tokens / < 500 lines) →
**resources** (bundled files, read as needed, referenced by path relative to the
skill root).

### 4.3 What clients do with it

**Claude Code** (<https://code.claude.com/docs/en/skills.md>) discovers
`~/.claude/skills/<name>/SKILL.md` (personal), `.claude/skills/<name>/SKILL.md`
(project, committed), and plugin-bundled skills. It injects **name + description
into the system prompt** at session start and exposes a **`Skill` tool** the model
calls to load a body — i.e. Claude Code uses *both* mechanisms Pi separates.
`/skill-name` is available to the user directly. Frontmatter can opt a skill out of
model invocation, leaving it user-only. On compaction, invoked skills are
re-injected up to a budget, oldest first.

`allowed-tools` enforcement in the Claude Code **CLI** is reported broken
(anthropics/claude-code [#37683](https://github.com/anthropics/claude-code/issues/37683),
[#67198](https://github.com/anthropics/claude-code/issues/67198)) while working in
the Agent SDK. **Treat `allowed-tools` as advisory, not a security boundary.**

**Codex CLI** adopted Agent Skills in December 2025
(<https://learn.chatgpt.com/docs/build-skills>, from
`developers.openai.com/codex/skills`). Same `name`/`description` frontmatter, same
"front-load the key use case and trigger words" advice. Explicit invocation is
`$skill-name` rather than `/skill-name`.

⚠️ **Codex's directory is not settled by our sources, and they conflict.** One
report has `~/.agents/skills/` (the harness-neutral path); Pi's own interop docs
tell users to add `~/.codex/skills` alongside `~/.claude/skills`. Neither is a
primary OpenAI doc. Two candidate paths and no authority is precisely why §6d is
declined: you cannot write a file into a convention you cannot name.

Note the ecosystem shape this implies: **`~/.agents/skills/` is emerging as the
vendor-neutral root** — Pi reads it unconditionally, and it is the one path both
reports agree exists. If Luma ever ships skills to disk, that is the target, not a
per-vendor directory.

### 4.4 Skills and MCP

There is no skills primitive in MCP. Two adjacent things exist:

- **Prompts** (`prompts/list`, `prompts/get`) — *user-controlled* templates with
  named arguments. Claude Code surfaces every server prompt as a slash command
  `/mcp__<server>__<prompt>`.
- **Resources** (`resources/list`, `resources/read`, RFC 6570 URI templates,
  `resources/subscribe`) — *application-controlled* data. Claude Code reaches them
  through `ListMcpResourcesTool` / `ReadMcpResourceTool`, i.e. only if the model
  decides to go looking. There is no automatic listing in context.

Community servers already map SKILL.md onto MCP as a `list_skills`/`get_skill`
tool pair (e.g. <https://github.com/DiscreteTom/agent-skills-mcp>), and there is an
open MCP discussion proposing skills supersede the prompts primitive
(<https://github.com/modelcontextprotocol/modelcontextprotocol/discussions/1779>).
Anthropic's own `mcp-server-dev` plugin ships skills + `references/`
(<https://modelcontextprotocol.io/docs/2026-07-28/develop/build-with-agent-skills>).

**Reading of the field:** the industry converged on the *format* (SKILL.md) and has
not converged on the *transport* for a remote skill. A tool pair is what everyone
actually ships; prompts are a free bonus in Claude Code; resources are inert unless
the model already knows to look.

---

## 5. Our implementation vs both

| Dimension | Luma today | Pi | Spec |
|---|---|---|---|
| Location | `src/shared/lib/agent/skills/` in the web bundle | any directory, recursive | `<root>/<name>/SKILL.md` |
| Discovery | Vite `import.meta.glob`, build-time | filesystem walk, runtime | filesystem |
| Frontmatter parse | hand-rolled `key: value`, no YAML | real YAML | YAML |
| `name` | required, unvalidated charset/length | defaults to dir name, validated | required, validated |
| `description` | optional (`?? ""`) | **required**, ≤1024 | **required**, ≤1024 |
| Body | required non-empty (throws) | not required | not specified |
| Bad skill | **throws, breaks the loader** | warning diagnostic | — |
| Listing | inside the **tool description** | **system prompt** `<available_skills>` | system prompt (both clients) |
| Loading | `skill` tool returns body | model **reads the file** at `<location>` | `Skill` tool (CC) / auto (Codex) |
| Level-3 resources | **impossible** (no paths, no reader) | free (paths + read tool) | `scripts/` `references/` `assets/` |
| `disable-model-invocation` | absent | supported | via client frontmatter |
| `license` / `compatibility` / `metadata` / `allowed-tools` | absent | ignored | optional |
| Reachable from Rust loop | **no** (though its prompt says yes) | — | — |
| Reachable from MCP | **no** | — | — |

### Gaps worth naming

1. **Nothing non-standard is in our frontmatter** — we use exactly `name` and
   `description`. That is the good news: the ten skills are already
   spec-conformant files. Only their *housing* is proprietary.
2. **`description` is optional in our loader** but is the entire trigger mechanism.
   A skill with no description silently becomes `- name: ` in the tool description.
   The test asserts every bundled skill has one; the type does not.
3. **A malformed skill throws at module scope.** `new SkillLoader()` runs
   `fromRaw` over every bundled source in the constructor, so one bad `SKILL.md`
   takes down the whole agent surface rather than one playbook. Pi's diagnostics
   model is strictly better and costs nothing.
4. **No level-3 resources, and no path to them.** Skills cross-reference each other
   by *bare name* today — `heavy-bass/SKILL.md:17` says "the modulation recipe in
   `finding-things-in-audio`", `rig-craft/SKILL.md:48` says "see
   contrast-and-darkness". Under a tool-based scheme the model has to guess that
   these are `skill` arguments. Under Pi's scheme they would be paths and the
   ambiguity disappears.
5. **The listing is in a tool description, so it is not visible to a loop that has
   no such tool** — which is exactly the Rust loop's and MCP's problem. A system-prompt
   listing is transport-independent; a tool description is not.
6. **Prompt-cache coupling.** `skillToolDescription` is derived from the loader at
   `get description()`, and Rust keys prompt caching on the serialized tool list
   (`tools/mod.rs:85-86`) and on a byte-stable system prompt
   (`agent/mod.rs:88`). Whatever we build must be **statically derived** — same
   bytes every run — or it silently breaks caching. Sorting by path in
   `bundled-skills.ts` already shows awareness of this.

---

## 6. Exposing skills to external agents — the four options

Evaluated against: standard-conformance, what Claude Code and Codex *actually do*
today, progressive disclosure, and one-source-of-truth with the in-app loop.

### (a) MCP prompts — `prompts/list` + `prompts/get`

Each skill becomes a prompt; the body is the returned message.

- **Conformance:** MCP-native, no invention. But prompts are *user-controlled*: the
  spec's own framing is "templates the user picks", not "instructions the model
  discovers".
- **Claude Code today:** genuinely good — every server prompt becomes
  `/mcp__luma__color`, discoverable in the slash menu with the description.
- **Codex today:** unverified. Do not count on it.
- **Progressive disclosure:** level 1 lands in the *client's command list*, not the
  model's context. The model never sees the listing, so it cannot decide to reach
  for a skill; only the human can. That is a real loss for an agent working
  unattended, and unattended is the MCP use case.
- **Cost:** `route()` gains two methods; both are answerable from static data.
- **Verdict:** worth shipping — as a *bonus surface for humans*, never as the only one.

### (b) MCP resources — `resources/list` + `resources/read`

Each skill becomes `skill://color` or `file://…/color/SKILL.md`.

- **Conformance:** exactly what resources are for (application-controlled content),
  and URI templates would even model `references/` sub-files properly.
- **Claude Code today:** the model must call `ListMcpResourcesTool` unprompted.
  Nothing puts the skills in front of it. In practice this means never.
- **Codex today:** unverified.
- **Progressive disclosure:** level 1 is *not delivered at all*. This is the
  failure mode — a perfectly conformant surface that no agent looks at.
- **Verdict:** no. Correct and inert. Revisit only if clients start auto-listing
  resources.

### (c) A `skill` tool on `luma-mcp`

`skill {name}` → the body, with the listing enumerated in the tool description,
plus the same listing folded into `open`'s reply.

- **Conformance:** not a *standard* skills transport, but it is what every shipped
  skills-over-MCP server does today, and it is exactly how Claude Code's own
  built-in `Skill` tool behaves.
- **Claude Code / Codex today:** works, identically, with zero client cooperation
  beyond MCP tool support. This is the only option true of both.
- **Progressive disclosure:** level 1 arrives in the tool list (always in context);
  level 2 on call. Level 3 needs the client's own file reader — and an external
  coding agent *has* one, so shipping the skills to disk (§7) makes level 3 real
  for MCP clients even though it is not for the in-app loop.
- **One source of truth:** trivially — same registry as the in-app loop, same
  bodies, same listing text.
- **Verdict:** **yes.** This is the load-bearing surface.

`open` is the right second injection point: it already returns the catalog, an MCP
client always calls it first, and it is the closest thing this server has to a
"session system prompt".

### (d) Writing `.claude/skills` / `.agents/skills` into the user's project on `open`

- **Conformance:** maximal — the client's own native skills machinery does the
  work, including `/skill-name`, compaction re-injection, and level-3 files.
- **Reality:** it writes files into a directory Luma does not own, from a tool call
  the user did not frame as a write. It litters, it goes stale, it collides with a
  real `.claude/skills/color/`, it requires knowing each client's path (and §4.3's
  Codex path is unverified), and it silently changes the behaviour of every future
  session in that repo. `open` is documented as a *read* that binds a session.
- **Verdict:** **no.** This is the "bolt a feature onto the wrong layer because
  it's faster" case, and the layer is somebody else's filesystem. If a user wants
  Luma's skills as native Claude Code skills, that is a deliberate
  `luma skills install` CLI action, not a side effect of `open`.

### Summary

| | conformance | CC today | Codex today | prog. disclosure | one source |
|---|---|---|---|---|---|
| (a) prompts | high | ✅ slash commands | ❓ | human-only level 1 | ✅ |
| (b) resources | high | ❌ inert | ❓ | ✅ none delivered | ✅ |
| (c) `skill` tool | de-facto | ✅ | ✅ | ✅ | ✅ |
| (d) write to disk | maximal | ✅ | ❓ path | ✅ | ❌ drifts |

**Ship (c), add (a) because it is nearly free and Claude Code does something nice
with it. Decline (b) and (d).**

A fifth position exists and is worth naming to reject it: **Pi's** — don't serve
skills over MCP because don't serve *anything* over MCP; ship a CLI and a README
and let the agent's own `bash` do the work (§3.5). It is internally consistent, and
it does not apply here. A README cannot be a sandboxed kernel holding a live
binding manifest for the track currently open. `luma-mcp` exists because the thing
we are handing over is not expressible as a command-line tool, and once an external
agent is holding that kernel it should hold the craft that goes with it.

---

## 7. Design it twice

### Design A — "the webview keeps the skills, Rust asks for them"

Leave the `SKILL.md` files under `src/shared/lib/agent/skills/`, keep
`import.meta.glob`, and give the Rust loop and `luma-mcp` access by having them
ask the frontend. Add a `list_skills` / `get_skill` Tauri command implemented in
TypeScript.

*Why it looks attractive:* zero migration, no new bundling, the existing loader and
its tests survive untouched.

*Why it fails:* the frontend is not a service the backend can call — `luma-mcp` and
`agent_harness` boot `luma_lib` with **no webview at all**, so there is nothing to
ask. The dependency arrow points the wrong way. It also leaves the content inside a
JS bundle, where an external coding agent can never read a `references/` file, and
it makes the Rust loop's skill surface depend on which process it is running in.
Ousterhout: an upward dependency plus information leakage of "skills are a
frontend concept" into three consumers that are not the frontend.

*Rejected.*

### Design B — "one on-disk source, Rust registry, three thin consumers" ✅

Skills are files. Rust discovers and validates them once. Everything else is a
projection of that registry.

```
resources/skills/<name>/SKILL.md          # + optional references/, scripts/, assets/
        │
        ▼
src-tauri/src/agent/skills.rs             # Skill { name, description, body, path }
   discover · validate · diagnostics       # the one registry
        │
        ├── agent/mod.rs      system_prompt() += <available_skills> listing   (Rust loop)
        ├── agent/tools/skill.rs           the `skill` tool                   (Rust loop)
        ├── commands/skills.rs  list_skills() → Vec<SkillMeta>, get_skill(name) → String
        │        └── src/shared/lib/agent/skills/  thin TS client, same listing + tool
        └── bin/luma-mcp.rs   `skill` tool + listing in `open` + prompts/list|get
```

*Why it wins:* it matches the precedent already set by `python-tool.md` and
`resources/fixtures/**` — bundled content lives on disk, Rust reads it, and every
host gets the same bytes. It puts level-3 resources within reach for the consumer
that can actually use them (an external coding agent with a file reader). It makes
the Rust loop's prompt honest. And the registry is a **deep module**: a directory
walk and a validator behind `SkillRegistry::load()` / `.listing()` / `.get(name)`.

*Cost:* the skills move; the TS loader shrinks to a client; Rust gains frontmatter
parsing (there is no YAML dep on that side today — see §8.3).

**Design B is the recommendation.**

---

## 8. The recommendation in detail

### 8.1 One on-disk skill source

`resources/skills/<name>/SKILL.md`, sibling to `resources/fixtures/` and
`resources/meshes/`, added to `tauri.conf.json` `resources` (`src-tauri/tauri.conf.json:54-61`).

Why `resources/` and not `src-tauri/src/agent/skills/`:

- fixtures already established that **bundled content authored as data** lives in
  the repo-root `resources/`, resolved through `app.path().resource_dir()` with
  `python_env::ensure_python_resource_dir_at`'s dev-tree-then-bundle fallback and
  the `LUMA_RESOURCE_DIR` override the headless hosts already use;
- it keeps skills editable by a lighting designer without touching `src/`;
- it leaves the door open to **user skills** at
  `$APPCONFIG/skills/<name>/SKILL.md`, discovered on top of the bundled set with
  later-wins-by-name — the same precedence rule the current `SkillLoader.load()`
  already implements, and the same one Claude Code uses for user vs project scope.

Frontmatter stays exactly `name` + `description` — spec-conformant, nothing
proprietary. `license` / `compatibility` / `metadata` are parsed-and-ignored so a
third-party skill dropped in does not warn. `allowed-tools` is **not** honoured:
the in-app agent has one or two tools, Claude Code's CLI enforcement is reported
broken (§4.3), and Pi documents the field without implementing it at all (§3.2).
Two reference implementations with a silent-failure stub in the same field is
enough evidence — accepting a field we do not enforce would be worse than
rejecting it. `disable-model-invocation` we *do* honour: it costs one filter in
`listing()` and it is how a skill becomes user-only later.

### 8.2 One Rust registry

`src-tauri/src/agent/skills.rs`:

```rust
/// A lighting-craft playbook, discovered from disk at startup.
pub struct Skill {
    pub name: String,        // == parent directory name
    pub description: String, // required; the model's only pre-load signal
    pub body: String,
    pub path: PathBuf,       // model-visible location; base for relative refs
}

pub struct SkillRegistry { /* … */ }

impl SkillRegistry {
    /// Discover every skill under `roots`, later roots winning by name.
    /// Never fails: an unreadable or invalid skill becomes a diagnostic.
    pub fn load(roots: &[PathBuf]) -> (Self, Vec<SkillDiagnostic>);

    /// The `<available_skills>` block for the system prompt. Byte-stable:
    /// entries are sorted by name, so prompt caching survives a reload.
    pub fn listing(&self) -> &str;

    pub fn get(&self, name: &str) -> Option<&Skill>;
    pub fn iter(&self) -> impl Iterator<Item = &Skill>;
}
```

Copy Pi's rules (§3.2): name ≤64 `[a-z0-9-]+`, description required and ≤1024,
`SKILL.md` short-circuits recursion, missing roots skipped, everything else a
warning. Adopt Pi's **deliberate leniency** on name-vs-directory too: a mismatch
warns, it does not drop, because that spec rule is hostile to skill directories
shared between harnesses.

**Diagnostics, not errors** — the single most valuable thing to take from Pi, and a
straight "define errors out of existence": the agent surface cannot be broken by a
bad playbook. Only one condition drops a skill entirely, and it is the one that
makes a skill meaningless: a missing or empty `description`.

Also copy Pi's **seam**: `SkillRegistry::load(roots)` takes an explicit root list
and knows nothing about where roots come from. Which roots exist — bundled,
`$APPCONFIG`, a future user setting — is a decision one caller makes at boot.
Discovery policy above, parsing below; split by knowledge, not by chronology.

Loaded once at boot, held in `AppServices` next to the other bundled catalogs.

### 8.3 Frontmatter in Rust

There is no YAML crate anywhere in `src-tauri/` today. Two options:

- **`serde_yaml`-family dependency** — full YAML, matches Pi exactly, ~1 new crate
  tree.
- **~30 lines hand-rolled** — split on the leading `---` / first `\n---`, parse
  `key: value` with quote stripping, exactly as `src/shared/lib/agent/frontmatter.ts`
  does today.

Recommend **hand-rolled**, matching the taste that produced `mcp-stdio` (207 lines
rather than an SDK): the spec's required fields are two flat strings, our skills use
no block scalars or nested maps, and a third-party skill with exotic YAML degrades
to a diagnostic rather than a panic. Reconsider if `metadata` (a nested map) ever
becomes load-bearing.

### 8.4 How the Pi-based in-app loop consumes it — mirror Pi exactly

Two changes, both making us *more* like Pi, not less:

**1. Move the listing from the tool description to the system prompt.**
`AgentKind::system_prompt()` becomes `system_prompt(&SkillRegistry) -> String`,
returning `include_str!("prompts/track.md")` + `"\n\n"` + `registry.listing()`.
Byte-stability is preserved because the listing is sorted and derived from bundled
files — same bytes every run, so the cached prefix still hits. The block is Pi's,
verbatim in shape:

```xml
<available_skills>
  <skill><name>…</name><description>…</description><location>…</location></skill>
</available_skills>
```

`<location>` is the real on-disk path. It is honest for MCP clients (they can read
it) and inert for the in-app loop (it cannot) — which is fine, because:

**2. Keep the `skill` tool as the in-app loading mechanism.**
Pi's model reads the file with a read tool. Our in-app model has exactly one tool
and it is `python` in a seatbelt sandbox whose only readable roots are the
workspace, the venv and `extra_read_roots`
(`agent_execution/worker_launcher.rs:28-42`). Giving the model a file reader for
skills would mean either a second tool or granting the sandbox a new read root and
teaching the model to `open()` a path — a worse interface than a named lookup, and
one more place the sandbox policy has to be right.

There is a second reason, and Pi supplies it: its own docs admit that under the
read-the-file scheme "models don't always do this" and recommend forcing the load
with `/skill:name` (§3.4). A named tool call is a harder affordance than a path in
a prompt. We get Pi's disclosure model *and* a reliable trigger.

So: **Pi's listing, Pi's `<skill>` framing, our tool for the fetch.** The tool
returns `formatSkillInvocation`'s exact envelope —

```
<skill name="color" location="/abs/…/color/SKILL.md">
References are relative to /abs/…/color.

<body>
</skill>
```

— so the model sees the same thing it would see in Pi, including the path rule that
makes level-3 references resolvable *if* the consumer can read files. One vocabulary,
two fetch mechanisms, chosen by what the host can actually do. The tool description
becomes short and static (no enumeration — the listing moved), which is strictly
better for prompt caching.

This also settles smell #1: `AgentKind::TrackCopilot` gets `SkillTool` in its
registry, and `prompts/track.md:25` becomes true.

*(Deliberate divergence from Pi, recorded: Pi has no skill tool. We add one because
our sandbox has no file reader. If the in-app agent ever gains one, delete the tool
and let the listing's `<location>` do the work — that is the endgame, and the
`<skill>` envelope is chosen so nothing else has to change when it happens.)*

**TypeScript side.** `src/shared/lib/agent/skills/` shrinks to a client of the two
Tauri commands: `buildSkillTool()` keeps its name, its chat label and its
`toModelOutput`, but `execute` becomes `invoke("get_skill", {name})`, and the
system prompt gains `await invoke("skills_listing")` in `buildSystem`. The
`SkillLoader` class, `bundled-skills.ts`, `import.meta.glob` and the frontmatter
parse all go. `frontmatter.ts` stays — `subagents/agent-loader.ts` still needs it.

### 8.5 How MCP exposes it

`luma-mcp` gains, in order of importance:

1. **A `skill` tool** — `skill {name}` → the same `<skill …>` envelope. Its
   description is the short static one plus the listing, because an MCP client has
   no Luma system prompt to put the listing in.
2. **The listing in `open`'s reply**, appended after the catalog. `open` is the
   session's de-facto preamble and every client calls it first.
3. **`prompts/list` + `prompts/get`**, so Claude Code users get
   `/mcp__luma__color` and friends in the slash menu.

For (3), `mcp-stdio::route` must answer two more methods. **Do not add `Routed`
variants** — the crate documents `Routed` as deliberately not `#[non_exhaustive]`
("a fourth would be a change every host must answer, not absorb",
`crates/mcp-stdio/src/lib.rs:44-46`), and both hosts (`bin/luma-mcp.rs:40-42`,
`gpui/crates/agent/src/mcp.rs:40-42`) match it exhaustively. Skills are **static
content**, exactly like the `tools` array, so they belong in the same class:

```rust
pub fn route(line: &str, info: ServerInfo, surface: &Surface) -> Routed
```

where `Surface { tools: &Value, prompts: &Value }` (prompts defaulting to empty,
`capabilities.prompts` declared only when non-empty). `prompts/list` and
`prompts/get` are then answered inside `route` from the passed-in data, the way
`tools/list` already is, and no host has to grow a match arm. The GPUI harness
passes an empty prompt set and is unaffected. The existing test asserting
`resources/list` → `-32601` stays true, which is the right outcome: we are
declining (b).

`PROTOCOL_VERSION` stays `2024-11-05` — prompts are in that revision.

### 8.6 Where this leaves the prompt duplication

Not this document's job to fix, but §8.4 forces the issue: `system_prompt()` gains
a parameter and a suffix, and the TS copy in `build-context.ts` must gain the same
suffix. Doing that twice, in two languages, on prose that is *already* byte-duplicated,
is how smell #2 becomes permanent. **Fix it in the same pass:** `build-context.ts`
imports `../../../../src-tauri/src/agent/prompts/track.md?raw` exactly as
`python-tool.ts` imports `python-tool.md?raw`, and keeps only the `## Current track`
interpolation. That is a ~60-line deletion and it removes an entire class of drift.

---

## 9. Migration list

| File | Action | Rough size |
|---|---|---|
| `resources/skills/<name>/SKILL.md` × 10 | **move** from `src/shared/lib/agent/skills/*/` unchanged (already spec-conformant) | 10 files, ~33 KB, no edits |
| `src-tauri/tauri.conf.json` | add `"../resources/skills/**/*"` to `resources` | 1 line |
| `src-tauri/src/agent/skills.rs` | **new** — `Skill`, `SkillRegistry`, `SkillDiagnostic`, discovery, validation, frontmatter, `listing()` | ~260 lines + ~120 test |
| `src-tauri/src/agent/tools/skill.rs` | **new** — `SkillTool` (`Tool` impl, `<skill …>` envelope) | ~70 lines |
| `src-tauri/src/agent/tools/mod.rs` | register `SkillTool` for `TrackCopilot` (before `PythonTool`? no — after, ordering is cache-keyed, pick once and freeze) | ~5 lines |
| `src-tauri/src/agent/mod.rs` | `system_prompt(&SkillRegistry) -> String`; hold the registry | ~20 lines |
| `src-tauri/src/agent/turn.rs:167` | pass the registry through | ~3 lines |
| `src-tauri/src/commands/skills.rs` | **new** — `skills_listing()`, `list_skills()`, `get_skill(name)` | ~50 lines |
| `src-tauri/src/dispatch.rs` + `lib.rs` | register the three commands | ~6 lines |
| `src-tauri/crates/mcp-stdio/src/lib.rs` | `Surface { tools, prompts }`, `prompts/list`, `prompts/get`, `prompt()` builder, capability advertisement | ~70 lines + tests |
| `src-tauri/src/bin/luma-mcp.rs` | `skill` tool + dispatch arm; listing appended to `open`; build the prompts array from the registry | ~60 lines |
| `gpui/crates/agent/src/mcp.rs` | pass an empty prompt set to `route` | ~2 lines |
| `src/shared/lib/agent/skills/skill-loader.ts` | **rewrite** as a thin client of `get_skill` / `skills_listing`; keep `buildSkillTool`, drop `SkillLoader`/`normalize`/`fromRaw` | 130 → ~45 lines |
| `src/shared/lib/agent/skills/bundled-skills.ts` | **delete** | −18 lines |
| `src/shared/lib/agent/skills/skill-loader.test.ts` | **rewrite** — parse/validation cases move to Rust; keep the tool-shape cases against a stubbed invoke | 140 → ~60 lines |
| `src/features/track-editor/agent/build-context.ts` | append the listing; **and** collapse the duplicated prose to `track.md?raw` (§8.6) | −60 lines |
| `src-tauri/src/agent/prompts/track.md:25` | reword to match the new mechanism | 1 line |
| `scripts/headless/mcp_smoke.ts` | cover `skill` and `prompts/list` | ~20 lines |

**Rough total: ~600 new/changed lines, ~220 deleted, 10 files moved.** The bulk is
the Rust registry and its tests; nothing here is architecturally risky, and every
piece has an existing precedent in the tree (`resources/fixtures` for bundling,
`python-tool.md` for cross-boundary sharing, `mcp-stdio`'s static `tools` array for
the prompts surface).

### Sequencing

1. `skills.rs` + tests, skills moved to `resources/skills/`, nothing wired.
2. Rust loop: `SkillTool` + system-prompt listing. Smell #1 closed.
3. Tauri commands; TS loader becomes a client; `bundled-skills.ts` deleted.
4. `build-context.ts` collapsed onto `track.md?raw`. Smell #2 closed.
5. `luma-mcp`: `skill` tool + `open` listing.
6. `mcp-stdio`: `Surface` + prompts; `luma-mcp` prompt array; smoke test.

Steps 1–3 are the whole in-app win; 5–6 are the external-agent win and can land
separately.

---

## 10. Open questions

- **User skills.** `$APPCONFIG/skills/` is designed for above but not required by
  anything yet. Decide whether it ships in the first pass — it costs one more root
  in `SkillRegistry::load` and a `luma skills install` story for (d)-by-consent.
- **Level-3 resources.** No skill uses `references/` today, and the in-app agent
  cannot read them at all. The design leaves room; do not build the room until a
  skill needs it. When one does, the sandbox's `extra_read_roots` is the seam.
- **Cross-references by bare name** (`heavy-bass:17`, `rig-craft:48`) should become
  either explicit "load the `X` skill" phrasing or real relative paths once
  `<location>` exists. Pick one — currently they read as neither.
- **Graph agent.** `AgentKind::PatternGraph` shares the notebook and would share the
  skill tool. Are the ten lighting playbooks right for it, or does it want its own
  set? A `metadata.agent-kinds` filter is the obvious extension point and the
  obvious over-engineering; do it only when the second set exists.
- **Codex's skill directory** (§4.3) is unverified and our two sources disagree
  (`~/.agents/skills` vs `~/.codex/skills`). Irrelevant while we decline (d), and
  blocking if that decision is ever revisited. If a `luma skills install` ever
  ships, target `~/.agents/skills/` and let the user point their harness at it.
- **Trust.** Pi gates project-local skills behind an explicit trust prompt because
  a skill can instruct a model to run anything (§3.1). Bundled skills need no gate;
  the moment `$APPCONFIG/skills/` or any user-supplied root exists, this question
  becomes real.
