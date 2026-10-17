# Codex account and article AI

In **Settings → Advanced → AI summaries, chat and translation**, select **Codex / ChatGPT**, then **Sign in with ChatGPT**. Complete the official browser sign-in. You can cancel or reopen the sign-in page from Papr. The account row shows your sign-in status; model and reasoning effort are separate rows.

The model list and supported reasoning levels come from the installed official Codex CLI. **Account default** and **Model default** follow Codex's defaults. Selecting a different model resets the reasoning level to its default so an unsupported level is not carried across. The model and reasoning settings apply to summaries, article conversations, connectivity tests and the **LLM** translation engine. Google, Bing and DeepL translation use their own engines. Model listing does not guarantee entitlement; a real request verifies access. Higher reasoning can take longer and uses the account's Codex allowance.

Papr uses the official [Codex app-server protocol](https://learn.chatgpt.com/docs/app-server) for local sign-in and streamed text. Credentials live in a dedicated `codex/` folder inside Papr's application data directory, outside SQLite and WebDAV. Papr does not read or change your terminal's `~/.codex` credentials. Signing out affects Papr's account only. Generation uses an ephemeral thread in an empty directory, read-only sandbox, disabled command execution, subagents, web search and apps. Approval requests are declined. Articles are source data, not tool instructions.

## Runtime

Install a current official [Codex CLI](https://developers.openai.com/codex/cli), for example:

```sh
npm install -g @openai/codex
```

Restart Papr after installation. Papr looks for `codex` on PATH and common installation paths, or in `<Papr application data>/codex-bin/codex`. For a custom installation, launch Papr with `PAPR_CODEX_BIN` set to the executable's absolute path. This repository does not embed the CLI binary in release bundles. On Windows, use the native `codex.exe` executable, not an npm `.cmd` shim.

## Reading

Open any article with body text and press **I** to open Article AI. Use **Summary** or **Chat**. Chat includes that article's text and recent follow-up turns. Each article has its own conversation, retained for the running app session; **Clear chat** resets it. Conversation history is bounded, and very long article text is truncated. Chat history is not synced.

Summaries open with a short explanation followed by descriptive sections. When relevant, they can reference up to two original article image URLs. Image URLs are supplied from the article and checked again before display; clicking an image opens its original link. The model receives links and captions, not image pixels, and cannot fetch or visually analyze them. No generated image URLs are allowed. Articles without relevant images receive a text summary.

In **Settings → Advanced → AI summary prompt**, edit and save your own instructions for structure, tone and length. The editor shows the effective prompt, supports **Restore default prompt**, and saves locally for all AI providers. Article text and original image URLs are attached automatically; no template placeholders are needed. Empty prompts fall back to the default.

Existing summaries and translations remain cached when you change models, reasoning or the summary prompt. Use **Regenerate summary** or re-run translation to replace the old result. AI output may be inaccurate. Using AI sends article text and, for summaries, original image links/captions to the selected provider.
