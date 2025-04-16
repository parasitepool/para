-- CreateTable
CREATE TABLE "PoolStats" (
    "id" SERIAL NOT NULL,
    "timestamp" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "runtime" INTEGER NOT NULL,
    "users" INTEGER NOT NULL,
    "workers" INTEGER NOT NULL,
    "idle" INTEGER NOT NULL,
    "disconnected" INTEGER NOT NULL,
    "hashrate1m" BIGINT NOT NULL DEFAULT 0,
    "hashrate5m" BIGINT NOT NULL DEFAULT 0,
    "hashrate15m" BIGINT NOT NULL DEFAULT 0,
    "hashrate1hr" BIGINT NOT NULL DEFAULT 0,
    "hashrate6hr" BIGINT NOT NULL DEFAULT 0,
    "hashrate1d" BIGINT NOT NULL DEFAULT 0,
    "hashrate7d" BIGINT NOT NULL DEFAULT 0,
    "diff" DOUBLE PRECISION NOT NULL,
    "accepted" BIGINT NOT NULL,
    "rejected" BIGINT NOT NULL,
    "bestshare" BIGINT NOT NULL,
    "SPS1m" DOUBLE PRECISION NOT NULL,
    "SPS5m" DOUBLE PRECISION NOT NULL,
    "SPS15m" DOUBLE PRECISION NOT NULL,
    "SPS1h" DOUBLE PRECISION NOT NULL,

    CONSTRAINT "PoolStats_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "User" (
    "address" TEXT NOT NULL,
    "createdAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updatedAt" TIMESTAMP(3) NOT NULL,
    "authorised" BIGINT NOT NULL DEFAULT 0,

    CONSTRAINT "User_pkey" PRIMARY KEY ("address")
);

-- CreateTable
CREATE TABLE "UserStats" (
    "id" SERIAL NOT NULL,
    "userAddress" TEXT NOT NULL,
    "hashrate1m" BIGINT NOT NULL DEFAULT 0,
    "hashrate5m" BIGINT NOT NULL DEFAULT 0,
    "hashrate1hr" BIGINT NOT NULL DEFAULT 0,
    "hashrate1d" BIGINT NOT NULL DEFAULT 0,
    "hashrate7d" BIGINT NOT NULL DEFAULT 0,
    "lastShare" BIGINT NOT NULL DEFAULT 0,
    "workerCount" INTEGER NOT NULL DEFAULT 0,
    "shares" BIGINT NOT NULL DEFAULT 0,
    "bestShare" DOUBLE PRECISION NOT NULL DEFAULT 0,
    "bestEver" BIGINT NOT NULL DEFAULT 0,
    "timestamp" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT "UserStats_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "Worker" (
    "id" SERIAL NOT NULL,
    "name" TEXT NOT NULL,
    "createdAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updatedAt" TIMESTAMP(3) NOT NULL,
    "hashrate1m" BIGINT NOT NULL DEFAULT 0,
    "hashrate5m" BIGINT NOT NULL DEFAULT 0,
    "hashrate1hr" BIGINT NOT NULL DEFAULT 0,
    "hashrate1d" BIGINT NOT NULL DEFAULT 0,
    "hashrate7d" BIGINT NOT NULL DEFAULT 0,
    "lastUpdate" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "shares" BIGINT NOT NULL DEFAULT 0,
    "bestShare" DOUBLE PRECISION NOT NULL DEFAULT 0,
    "bestEver" BIGINT NOT NULL DEFAULT 0,
    "userAddress" TEXT NOT NULL,

    CONSTRAINT "Worker_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "WorkerStats" (
    "id" SERIAL NOT NULL,
    "workerId" INTEGER NOT NULL,
    "hashrate1m" BIGINT NOT NULL DEFAULT 0,
    "hashrate5m" BIGINT NOT NULL DEFAULT 0,
    "hashrate1hr" BIGINT NOT NULL DEFAULT 0,
    "hashrate1d" BIGINT NOT NULL DEFAULT 0,
    "hashrate7d" BIGINT NOT NULL DEFAULT 0,
    "shares" BIGINT NOT NULL DEFAULT 0,
    "bestShare" DOUBLE PRECISION NOT NULL DEFAULT 0,
    "bestEver" BIGINT NOT NULL DEFAULT 0,
    "timestamp" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT "WorkerStats_pkey" PRIMARY KEY ("id")
);

-- CreateIndex
CREATE UNIQUE INDEX "User_address_key" ON "User"("address");

-- CreateIndex
CREATE INDEX "UserStats_timestamp_idx" ON "UserStats"("timestamp");

-- CreateIndex
CREATE UNIQUE INDEX "Worker_userAddress_name_key" ON "Worker"("userAddress", "name");

-- CreateIndex
CREATE INDEX "WorkerStats_timestamp_idx" ON "WorkerStats"("timestamp");

-- AddForeignKey
ALTER TABLE "UserStats" ADD CONSTRAINT "UserStats_userAddress_fkey" FOREIGN KEY ("userAddress") REFERENCES "User"("address") ON DELETE RESTRICT ON UPDATE CASCADE;

-- AddForeignKey
ALTER TABLE "Worker" ADD CONSTRAINT "Worker_userAddress_fkey" FOREIGN KEY ("userAddress") REFERENCES "User"("address") ON DELETE RESTRICT ON UPDATE CASCADE;

-- AddForeignKey
ALTER TABLE "WorkerStats" ADD CONSTRAINT "WorkerStats_workerId_fkey" FOREIGN KEY ("workerId") REFERENCES "Worker"("id") ON DELETE RESTRICT ON UPDATE CASCADE;
