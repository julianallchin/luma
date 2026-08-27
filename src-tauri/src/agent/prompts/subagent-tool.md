Delegate one self-contained piece of work to a subagent: a fresh conversation
with your tools, your model and an empty context, working on a private copy of
the authored document.

Use it when the work is worth its own context — a search whose intermediate
output you do not want to read, an experiment you might discard, a change you
want made and summarized rather than narrated. Do not use it for a step you
could take in one cell; a subagent costs a whole turn.

The subagent cannot see this conversation. `task` is everything it will know,
so write it as a standalone brief: what to do, what "done" looks like, and what
to report back. It runs to completion and answers once; there is no way to talk
to it partway.

When it finishes, its edits are merged into the document you are editing and
you are given its final message. If the merge conflicts, or the subagent fails,
nothing is applied and you are told so — its work stays readable on its own
thread.
