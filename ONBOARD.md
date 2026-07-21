# Onboarding Guide for AI Assistants

If you are reading this file for the first time (fresh context), immediately follow the instructions below. Do not just summarize this file.

This guide helps new AI assistant instances quickly get the context needed to resume development on this project.

**Read only files explicitly named by the user or explicitly required by those files. Ask before reading anything else.**
**During onboarding, do not infer extra context files. Follow the explicit read list only.**

## Action Required

### Read files

Please read the following files one at a time, in the given order:

AGENTS.md
PRINCIPLES.md
DIAGNOSTICS.md
IN-PROG.md

Files these documents explicitly instruct you to read. If any of the above files contain explicit instructions to read a specific file (e.g., "Read the spec: filename"), follow that instruction. Do not follow casual mentions or "where to look" pointers.

### Flag errors and inconsistencies

If any of the files you've read contain errors or inconsistencies, let the user know.

### Summarize

Do this now before responding to the user: After reading all files above, briefly summarize the project based on what you've read in these files.
Once you've done this, the user will describe the scope of work for this session.

## Session feature implementation process

Features are applied one-by-one with user approval:
1. **Show current status**: Describe what has been done and what is scheduled to develop next
2. **Propose where to resume**: Propose next steps based on development plan. The user may request some other feature be worked on or bug addressed at this point. This does not remove the requirement to show your plan and receive explicit approval before making code changes. This approval is required regardless of what you are working on.
3. **Show plan and discuss**: Ask any questions you may have on the development task. The user may also have questions or request changes. Iterate until all questions have been answered.
4. **Display final plan including estimated development effort (in tokens) and confidence level the plan will work on the first try (in percentage) to the user for review and final approval**: Wait for final approval before beginning development. Approval must be explicit.
5. **Pause development when agreed scope is complete or you run into problems**: Do not continue if things go off-track. Instead, pause and discuss. When unsure, pause.
6. **End session procedure**: Once scoped-development is complete, summarize what has been done. The user will ask a series of questions and/or ask for changes before providing approval to log current status, which means update current status in the detailed plan.
