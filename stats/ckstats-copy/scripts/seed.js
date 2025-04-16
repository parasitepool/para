const { PrismaClient } = require('@prisma/client');

const prisma = new PrismaClient();

async function fetchPoolStats() {
  console.log('Fetching pool stats...');
  const apiUrl = process.env.API_URL || 'https://solo.ckpool.org';
  const response = await fetch(`${apiUrl}/pool/pool.status`);
  const data = await response.text();
  const jsonLines = data.split('\n').filter(Boolean);
  const parsedData = jsonLines.reduce((acc, line) => ({ ...acc, ...JSON.parse(line) }), {});
  return parsedData;
}

async function seed() {
  try {
    console.log('Fetching pool stats...');
    const stats = await fetchPoolStats();
    
    console.log('Saving pool stats to database...');

    // Function to convert hashrate with units to string
    const convertHashrate = (value) => {
      const units = { P: 1e15, T: 1e12, G: 1e9, M: 1e6, K: 1e3 };
      const match = value.match(/^(\d+(\.\d+)?)([PTGMK])$/);
      if (match) {
        const [, num, , unit] = match;
        return Math.round(parseFloat(num) * units[unit]).toString();
      }
      return value;
    };

    await prisma.poolStats.create({
      data: {
        runtime: stats.runtime,
        users: stats.Users,
        workers: stats.Workers,
        idle: stats.Idle,
        disconnected: stats.Disconnected,
        hashrate1m: convertHashrate(stats.hashrate1m),
        hashrate5m: convertHashrate(stats.hashrate5m),
        hashrate15m: convertHashrate(stats.hashrate15m),
        hashrate1hr: convertHashrate(stats.hashrate1hr),
        hashrate6hr: convertHashrate(stats.hashrate6hr),
        hashrate1d: convertHashrate(stats.hashrate1d),
        hashrate7d: convertHashrate(stats.hashrate7d),
        diff: stats.diff,
        accepted: BigInt(stats.accepted),
        rejected: BigInt(stats.rejected),
        bestshare: BigInt(stats.bestshare),
        SPS1m: stats.SPS1m,
        SPS5m: stats.SPS5m,
        SPS15m: stats.SPS15m,
        SPS1h: stats.SPS1h,
      },
    });

    console.log('Database seeded successfully');
  } catch (error) {
    console.error('Error seeding database:', error);
  } finally {
    await prisma.$disconnect();
  }
}

seed().catch((error) => {
  console.error('Error in seed function:', error);
  process.exit(1);
});
