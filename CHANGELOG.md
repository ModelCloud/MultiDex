The changelog can be found on the [releases page](https://github.com/openai/codex/releases).

## Unreleased

- Compress Responses request bodies with zstd for the Lite-backed MultiDex providers by default, negotiating per endpoint and resending uncompressed once when a server cannot decode them; OpenAI and remote providers are unchanged. Add the `request_compression` (`auto`/`zstd`/`none`) and opt-in `inherits_tools_on_continuation` provider options, the latter omitting unchanged tools on `previous_response_id` continuations.
- Fix missing command lifecycle events when unified exec cannot create a process, preserving approval rejection and sandbox retry behavior. Thanks @Marvinthebored ([#44557](https://github.com/openai/codex/issues/44557)).
