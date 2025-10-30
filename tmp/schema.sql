CREATE TABLE IF NOT EXISTS accounts
(
    id               BIGSERIAL PRIMARY KEY,
    username         VARCHAR(128) NOT NULL UNIQUE,
    lnurl            VARCHAR(255),
    past_lnurls      JSONB                    DEFAULT '[]'::JSONB,
    total_diff       BIGINT                   DEFAULT 0,
    lnurl_updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    created_at       TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    updated_at       TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_accounts_username ON accounts (username);
CREATE INDEX IF NOT EXISTS idx_accounts_lnurl ON accounts (lnurl);
CREATE INDEX IF NOT EXISTS idx_accounts_past_lnurls ON accounts USING GIN (past_lnurls);

CREATE TABLE IF NOT EXISTS payouts
(
    id                BIGSERIAL PRIMARY KEY,
    account_id        BIGINT         NOT NULL REFERENCES accounts (id) ON DELETE RESTRICT,
    bitcoin_amount    BIGINT         NOT NULL, -- TODO: convert to sats
    diff_paid         BIGINT         NOT NULL,
    blockheight_start INTEGER        NOT NULL,
    blockheight_end   INTEGER        NOT NULL,
    status            VARCHAR(20)    NOT NULL  DEFAULT 'pending' CHECK (status IN ('pending', 'processing', 'success', 'failure', 'cancelled')),
    attempts          SMALLINT       NOT NULL  DEFAULT 0,
    failure_reason    TEXT,
    transaction_id    VARCHAR(64),
    created_at        TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    updated_at        TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    processed_at      TIMESTAMP WITH TIME ZONE,

    CONSTRAINT valid_blockheight_range CHECK (blockheight_end >= blockheight_start),
    CONSTRAINT unique_payout_per_block UNIQUE (account_id, blockheight_end)
);
CREATE INDEX IF NOT EXISTS idx_payouts_accounts_id ON payouts (account_id);
CREATE INDEX IF NOT EXISTS idx_payouts_status ON payouts (status);
CREATE INDEX IF NOT EXISTS idx_payouts_blockheight_range ON payouts (blockheight_start, blockheight_end);
CREATE INDEX IF NOT EXISTS idx_payouts_created_at ON payouts (created_at);
CREATE INDEX IF NOT EXISTS idx_payouts_user_status ON payouts (account_id, status);


CREATE OR REPLACE FUNCTION update_accounts_modified()
    RETURNS TRIGGER AS
$$
BEGIN
    NEW.updated_at = NOW();

    IF OLD.lnurl IS DISTINCT FROM NEW.lnurl THEN
        NEW.lnurl_updated_at = NOW();
        IF OLD.lnurl IS NOT NULL AND NOT (NEW.past_lnurls @> jsonb_build_array(OLD.lnurl)) THEN
            NEW.past_lnurls = NEW.past_lnurls || jsonb_build_array(OLD.lnurl);
        END IF;
    END IF;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER update_accounts_modtime
    BEFORE UPDATE
    ON accounts
    FOR EACH ROW
EXECUTE FUNCTION update_accounts_modified();

CREATE OR REPLACE FUNCTION update_payouts_modified()
    RETURNS TRIGGER AS
$$
BEGIN
    NEW.updated_at = NOW();

    IF OLD.status IS DISTINCT FROM NEW.status AND NEW.status IN ('success', 'failure') THEN
        NEW.processed_at = NOW();
    END IF;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER update_payouts_modtime
    BEFORE UPDATE
    ON payouts
    FOR EACH ROW
EXECUTE FUNCTION update_payouts_modified();