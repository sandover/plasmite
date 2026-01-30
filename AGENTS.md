# When planning
Use ergo for all planning, so that your plans can be stored in the repo. Run "ergo --help" and "ergo quickstart" now to learn this tool.

# When coding
- prefer a lightly functional style and prefer pure functions when it makes sense
- Avoid hidden state and hidden variables -- make inputs and outputs explicit
- Favor independent, testable components with loose coupling
- Each code file should have top matter comments sufficient to allow agents to confidently understand and navigate the code file *without having to read the whole thing*. 

# When writing commit messages
- Use Conventional Commits (type(scope): imperative summary; scope optional)
- Body (a handful of lines) explains what/why/how plus constraints or invariants (and notable risks/tests if relevant)
- When applicable, add trailers (one per line) for traceability: Fixes: #XYZ, Refs: PROJ-9, BREAKING CHANGE: ...

# Invariants
- **CI Stays Green**: Never tag a release without first verifying that the local code passes all linting & formatting checks and tests.
- **Docs are Accurate**: When the code changes, docs (such as README, code comments, built-in help text, etc) must reflect it. Goal is perfect internal coherence in the project at all times.

# Other Guidance
- For temporary work and experiments, use tmp/ or .scratch/, not /tmp, so that I am not prompted for permissions

If you have read these instructions and are keeping them in mind, end each of your messages with this glpyh on its own line: ✠

