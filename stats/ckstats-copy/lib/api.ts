import { PoolStats, User, UserStats, Worker } from '@prisma/client';

import prisma from './db';
import { convertHashrate } from '../utils/helpers';

const HISTORICAL_DATA_POINTS = 2880;

export type PoolStatsInput = Omit<PoolStats, 'id' | 'timestamp'>;

export type PoolStatsType = {
  id: number;
  timestamp: Date;
  runtime: number;
  users: number;
  workers: number;
  idle: number;
  disconnected: number;
  hashrate1m: bigint;
  hashrate5m: bigint;
  hashrate15m: bigint;
  hashrate1hr: bigint;
  hashrate6hr: bigint;
  hashrate1d: bigint;
  hashrate7d: bigint;
  diff: number;
  accepted: bigint;
  rejected: bigint;
  bestshare: bigint;
  SPS1m: number;
  SPS5m: number;
  SPS15m: number;
  SPS1h: number;
};

export async function savePoolStats(stats: PoolStatsInput): Promise<PoolStats> {
  return prisma.poolStats.create({
    data: stats,
  });
}

export async function getLatestPoolStats(): Promise<PoolStats | null> {
  return prisma.poolStats.findFirst({
    orderBy: { timestamp: 'desc' },
  });
}

export async function getHistoricalPoolStats(): Promise<PoolStats[]> {
  return prisma.poolStats.findMany({
    orderBy: { timestamp: 'desc' },
    take: HISTORICAL_DATA_POINTS,
  });
}

export async function getUserWithWorkersAndStats(
  address: string
): Promise<(User & { workers: Worker[]; stats: UserStats[] }) | null> {
  return prisma.user.findUnique({
    where: { address },
    include: {
      workers: {
        orderBy: { hashrate5m: 'desc' },
      },
      stats: {
        orderBy: { timestamp: 'desc' },
        take: 1,
      },
    },
  });
}

export async function getUserHistoricalStats(
  address: string
): Promise<UserStats[]> {
  return prisma.userStats.findMany({
    where: { userAddress: address },
    orderBy: { timestamp: 'desc' },
    take: HISTORICAL_DATA_POINTS,
  });
}

export async function getWorkerWithStats(
  userAddress: string,
  workerName: string
) {
  workerName = workerName.trim();

  return prisma.worker.findUnique({
    where: {
      userAddress_name: {
        userAddress,
        name: workerName,
      },
    },
    include: {
      stats: {
        orderBy: {
          timestamp: 'desc',
        },
        take: HISTORICAL_DATA_POINTS,
      },
    },
  });
}

export async function getTopUserDifficulties(limit: number = 10): Promise<
  {
    address: string;
    workerCount: number;
    difficulty: string;
    hashrate1hr: string;
    hashrate1d: string;
    hashrate7d: string;
    bestShare: string;
  }[]
> {
  const topUsers = await prisma.userStats.findMany({
    select: {
      userAddress: true,
      workerCount: true,
      bestEver: true,
      bestShare: true,
      hashrate1hr: true,
      hashrate1d: true,
      hashrate7d: true,
    },
    where: {
      user: {
        isPublic: true,
      },
    },
    orderBy: [{ bestEver: 'desc' }, { timestamp: 'desc' }],
    take: limit,
    distinct: ['userAddress'],
  });

  return topUsers.map((user) => ({
    address: user.userAddress,
    workerCount: user.workerCount,
    difficulty: user.bestEver.toString(),
    hashrate1hr: user.hashrate1hr.toString(),
    hashrate1d: user.hashrate1d.toString(),
    hashrate7d: user.hashrate7d.toString(),
    bestShare: user.bestShare.toString(),
  }));
}

export async function getTopUserHashrates(limit: number = 10): Promise<
  {
    address: string;
    workerCount: number;
    hashrate1hr: string;
    hashrate1d: string;
    hashrate7d: string;
    bestShare: string;
    bestEver: string;
  }[]
> {
  const userStats = await prisma.userStats.groupBy({
    by: ['userAddress'],
    _max: {
      id: true,
    },
    where: {
      user: {
        isPublic: true,
      },
    },
  });
  const topUsers = await prisma.userStats.findMany({
    select: {
      userAddress: true,
      workerCount: true,
      hashrate1hr: true,
      hashrate1d: true,
      hashrate7d: true,
      bestShare: true,
      bestEver: true,
    },
    where: {
      id: {
        in: userStats
          .map((user) => user._max.id)
          .filter((id): id is number => id !== null),
      },
      user: {
        isPublic: true,
      },
    },
    orderBy: [{ hashrate1hr: 'desc' }, { timestamp: 'desc' }],
    take: limit,
    distinct: ['userAddress'],
  });

  return topUsers.map((user) => ({
    address: user.userAddress,
    workerCount: user.workerCount,
    hashrate1hr: user.hashrate1hr.toString(),
    hashrate1d: user.hashrate1d.toString(),
    hashrate7d: user.hashrate7d.toString(),
    bestShare: user.bestShare.toString(),
    bestEver: user.bestEver.toString(),
  }));
}

export async function resetUserActive(address: string): Promise<void> {
  await prisma.user.update({
    where: { address },
    data: { isActive: true },
  });
}

export async function updateSingleUser(address: string): Promise<void> {
  const apiUrl = process.env.API_URL || 'https://solo.ckpool.org';
  if (!apiUrl) {
    throw new Error('API_URL is not defined in environment variables');
  }

  console.log('Attempting to update user stats for:', address);

  try {
    const response = await fetch(`${apiUrl}/users/${address}`);
    if (!response.ok) {
      throw new Error(`HTTP error! status: ${response.status}`);
    }
    const userData = await response.json();

    await prisma.$transaction(async (prisma) => {
      // Update or create user
      await prisma.user.upsert({
        where: { address },
        update: {
          authorised: BigInt(userData.authorised),
          isActive: true,
        },
        create: {
          address,
          authorised: BigInt(userData.authorised),
          isActive: true,
        },
      });

      // Create a new UserStats entry
      await prisma.userStats.create({
        data: {
          user: { connect: { address } },
          hashrate1m: convertHashrate(userData.hashrate1m),
          hashrate5m: convertHashrate(userData.hashrate5m),
          hashrate1hr: convertHashrate(userData.hashrate1hr),
          hashrate1d: convertHashrate(userData.hashrate1d),
          hashrate7d: convertHashrate(userData.hashrate7d),
          lastShare: BigInt(userData.lastshare),
          workerCount: userData.workers,
          shares: BigInt(userData.shares),
          bestShare: parseFloat(userData.bestshare),
          bestEver: BigInt(userData.bestever),
        },
      });

      // Update or create workers
      for (const workerData of userData.worker) {
        const workerName = workerData.workername.split('.')[1];
        await prisma.worker.upsert({
          where: {
            userAddress_name: {
              userAddress: address,
              name: workerName,
            },
          },
          update: {
            hashrate1m: convertHashrate(workerData.hashrate1m),
            hashrate5m: convertHashrate(workerData.hashrate5m),
            hashrate1hr: convertHashrate(workerData.hashrate1hr),
            hashrate1d: convertHashrate(workerData.hashrate1d),
            hashrate7d: convertHashrate(workerData.hashrate7d),
            lastUpdate: new Date(workerData.lastshare * 1000),
            shares: BigInt(workerData.shares),
            bestShare: parseFloat(workerData.bestshare),
            bestEver: BigInt(workerData.bestever),
          },
          create: {
            userAddress: address,
            name: workerName,
            hashrate1m: convertHashrate(workerData.hashrate1m),
            hashrate5m: convertHashrate(workerData.hashrate5m),
            hashrate1hr: convertHashrate(workerData.hashrate1hr),
            hashrate1d: convertHashrate(workerData.hashrate1d),
            hashrate7d: convertHashrate(workerData.hashrate7d),
            lastUpdate: new Date(workerData.lastshare * 1000),
            shares: BigInt(workerData.shares),
            bestShare: parseFloat(workerData.bestshare),
            bestEver: BigInt(workerData.bestever),
          },
        });
      }
    });

    console.log(`Updated user and workers for: ${address}`);
  } catch (error) {
    console.error(`Error updating user ${address}:`, error);
    throw error;
  }
}

export async function toggleUserStatsPrivacy(
  address: string
): Promise<{ isPublic: boolean }> {
  const user = await prisma.user.findUnique({
    where: { address },
    select: { isPublic: true },
  });

  if (!user) {
    throw new Error('User not found');
  }

  const newIsPublic = !user.isPublic;

  await prisma.user.update({
    where: { address },
    data: { isPublic: newIsPublic },
  });

  return { isPublic: newIsPublic };
}
