-- CreateIndex
CREATE INDEX "UserStats_userAddress_bestEver_timestamp_idx" ON "UserStats"("userAddress", "bestEver", "timestamp");

-- CreateIndex
CREATE INDEX "UserStats_userAddress_hashrate1hr_timestamp_idx" ON "UserStats"("userAddress", "hashrate1hr", "timestamp");
