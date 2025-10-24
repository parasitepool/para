CREATE OR REPLACE FUNCTION update_accounts_from_remote_shares()
    RETURNS TRIGGER AS $$
BEGIN
    INSERT INTO accounts (username, lnurl, total_diff, created_at, updated_at)
    VALUES (
        NEW.username,
        CASE
            WHEN NEW.lnurl IS NOT NULL AND TRIM(BOTH ' ' FROM NEW.lnurl) != ''
            THEN TRIM(BOTH ' ' FROM NEW.lnurl)
            ELSE NULL
        END,
        CASE WHEN NEW.result = true THEN NEW.diff ELSE 0 END,
        NOW(),
        NOW()
    )
    ON CONFLICT (username) DO UPDATE
    SET
        lnurl = CASE
            WHEN accounts.lnurl IS NULL
                AND EXCLUDED.lnurl IS NOT NULL
            THEN EXCLUDED.lnurl
            ELSE accounts.lnurl
        END,
        total_diff = accounts.total_diff + EXCLUDED.total_diff;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_update_accounts_on_remote_share
    AFTER INSERT ON remote_shares
    FOR EACH ROW
    WHEN (NEW.username IS NOT NULL AND NEW.username != '')
    EXECUTE FUNCTION update_accounts_from_remote_shares();

