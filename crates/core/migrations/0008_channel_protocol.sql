-- The protocol a channel speaks: the name the usage-adapter registry resolves
-- (issue #104, roadmap P1-2).
--
-- Every channel written so far is OpenAI-compatible, so the default is the truth for
-- all of them. The column names a protocol the adapter registry must know — writes are
-- refused by `Db::set_channel` for a protocol this build has no adapter for, because a
-- channel that cannot normalize its own usage reports cannot serve.

ALTER TABLE oxsum.channels
    ADD COLUMN protocol text NOT NULL DEFAULT 'openai';
