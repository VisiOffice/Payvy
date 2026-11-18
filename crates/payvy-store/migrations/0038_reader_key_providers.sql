-- Gemini, Grok and Muse beside Anthropic and OpenAI (reader_key.set). Each
-- provider's address is fixed in the server, never stored here.
ALTER TABLE reader_keys DROP CONSTRAINT reader_keys_provider_check;
ALTER TABLE reader_keys ADD CONSTRAINT reader_keys_provider_check
    CHECK (provider IN ('anthropic', 'openai', 'gemini', 'xai', 'meta'));
