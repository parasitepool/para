-- CreateIndex
CREATE INDEX "PoolStats_timestamp_idx" ON "PoolStats"("timestamp");

-- CreateIndex
CREATE INDEX "User_isActive_idx" ON "User"("isActive");

-- CreateIndex
CREATE INDEX "User_isPublic_idx" ON "User"("isPublic");

-- CreateIndex
CREATE INDEX "UserStats_userAddress_timestamp_idx" ON "UserStats"("userAddress", "timestamp");

-- CreateIndex
CREATE INDEX "Worker_userAddress_idx" ON "Worker"("userAddress");

-- CreateIndex
CREATE INDEX "WorkerStats_workerId_timestamp_idx" ON "WorkerStats"("workerId", "timestamp");
