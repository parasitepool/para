INSERT INTO accounts (username, lnurl, past_lnurls, total_diff, created_at, updated_at)
WITH latest_lnurl AS (SELECT DISTINCT ON (username) username,
                                                    TRIM(BOTH ' ' FROM lnurl) AS latest_lnurl,
                                                    id
                      FROM remote_shares
                      WHERE username IS NOT NULL
                        AND username != ''
                      ORDER BY username, id DESC),
     aggregated_data AS (SELECT TRIM(BOTH ' ' FROM rs.username) AS username,
                                ll.latest_lnurl,
                                jsonb_agg(DISTINCT TRIM(BOTH ' ' FROM rs.lnurl) ORDER BY TRIM(BOTH ' ' FROM rs.lnurl))
                                FILTER (WHERE rs.lnurl IS NOT NULL AND TRIM(BOTH ' ' FROM rs.lnurl) != ll.latest_lnurl) AS past_lnurls,
                                COALESCE(SUM(rs.diff), 0)                                                               AS total_diff
                         FROM remote_shares rs
                                  INNER JOIN latest_lnurl ll ON rs.username = ll.username
                         WHERE rs.username IS NOT NULL
                           AND rs.username != ''
                           AND rs.result = true
                         GROUP BY rs.username, ll.latest_lnurl)
SELECT username,
       latest_lnurl,
       COALESCE(past_lnurls, '[]'::jsonb) AS past_lnurls,
       total_diff,
       NOW()                              AS created_at,
       NOW()                              AS updated_at
FROM aggregated_data
ON CONFLICT (username) DO UPDATE
    SET lnurl            = EXCLUDED.lnurl,
        past_lnurls      = CASE
                               WHEN accounts.lnurl IS DISTINCT FROM EXCLUDED.lnurl
                                   AND accounts.lnurl IS NOT NULL
                                   AND NOT (accounts.past_lnurls @> jsonb_build_array(accounts.lnurl))
                                   THEN accounts.past_lnurls || jsonb_build_array(accounts.lnurl) ||
                                        EXCLUDED.past_lnurls
                               ELSE accounts.past_lnurls || EXCLUDED.past_lnurls
            END,
        total_diff       = EXCLUDED.total_diff,
        lnurl_updated_at = CASE
                               WHEN accounts.lnurl IS DISTINCT FROM EXCLUDED.lnurl
                                   THEN NOW()
                               ELSE accounts.lnurl_updated_at
            END,
        updated_at       = NOW();