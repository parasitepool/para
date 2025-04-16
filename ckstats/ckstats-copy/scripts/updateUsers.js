const { PrismaClient } = require('@prisma/client');

const prisma = new PrismaClient();

const convertHashrate = (value) => {
  const units = { P: 1e15, T: 1e12, G: 1e9, M: 1e6, K: 1e3 };
  // Updated regex to handle scientific notation
  const match = value.match(/^(\d+(\.\d+)?(?:e[+-]\d+)?)([PTGMK])$/i);
  if (match) {
    const [, num, , unit] = match;
    // Parse the number, which now handles scientific notation
    const parsedNum = parseFloat(num);
    return BigInt(Math.round(parsedNum * units[unit.toUpperCase()]));
  }
  return value;
};

async function fetchUserDataWithRetry(address, maxRetries = 3, delay = 500) {
  for (let attempt = 1; attempt <= maxRetries; attempt++) {
    try {
      // console.log(`Fetching data for ${address}...`);
      const apiUrl = process.env.API_URL || 'https://solo.ckpool.org';
      const response = await fetch(`${apiUrl}/users/${address}`);
      if (!response.ok) {
        throw new Error(`HTTP error! status: ${response.status}`);
      }
      return await response.json();
    } catch (error) {
      if (attempt === maxRetries) {
        console.error(`Failed to fetch data for ${address} after ${maxRetries} attempts.`);
        
        // Set user's isActive status to false
        await prisma.user.update({
          where: { address },
          data: { isActive: false },
        });
        
        throw error;
      }
      console.log(`Attempt ${attempt} failed for ${address}. Retrying...`);
      await new Promise(resolve => setTimeout(resolve, delay * attempt));
    }
  }
}

async function updateUser(address, userData) {
  await prisma.user.upsert({
    where: { address },
    update: {
      authorised: BigInt(userData.authorised),
    },
    create: {
      address,
      authorised: BigInt(userData.authorised),
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
}

async function updateWorker(address, workerData) {
  if (!workerData.workername) {
    console.log(`Worker data for address ${address} is missing a valid name. Skipping.`);
    return;
  }

  const workerName = workerData.workername.includes('.')
    ? workerData.workername.split('.')[1]
    : workerData.workername.includes('_')
      ? workerData.workername.split('_')[1]
      : '';

  const worker = await prisma.worker.upsert({
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

  // Create a new WorkerStats entry
  await prisma.workerStats.create({
    data: {
      workerId: worker.id,
      hashrate1m: convertHashrate(workerData.hashrate1m),
      hashrate5m: convertHashrate(workerData.hashrate5m),
      hashrate1hr: convertHashrate(workerData.hashrate1hr),
      hashrate1d: convertHashrate(workerData.hashrate1d),
      hashrate7d: convertHashrate(workerData.hashrate7d),
      shares: BigInt(workerData.shares),
      bestShare: parseFloat(workerData.bestshare),
      bestEver: BigInt(workerData.bestever),
    },
  });
}

async function updateUserAndWorkers(address) {
  try {
    const userData = await fetchUserDataWithRetry(address);
    await prisma.$transaction(async () => {
      await updateUser(address, userData,);
      await Promise.all(userData.worker.map(w => updateWorker(address, w)));
    });
    console.log(`Updated user and workers for: ${address}`);
  } catch (error) {
    console.error(`Error updating user ${address}:`, error);
  }
}

async function updateUsers() {
  try {
    const users = await prisma.user.findMany({
      where: { isActive: true },
      select: { address: true },
    });
    const batchSize = 4;

    for (let i = 0; i < users.length; i += batchSize) {
      const batch = users.slice(i, i + batchSize);
      await Promise.all(batch.map(user => updateUserAndWorkers(user.address)));
    }

    console.log('All users and workers updated successfully');
  } catch (error) {
    console.error('Error updating users:', error);
  } finally {
    await prisma.$disconnect();
    // Force garbage collection if running in Node.js with the --expose-gc flag
    if (global.gc) {
      global.gc();
    }
  }
}

(async () => {
  try {
    await updateUsers();
  } catch (error) {
    console.error('Unhandled error:', error);
  } finally {
    // Ensure that Prisma client is disconnected
    await prisma.$disconnect();
    // Ensure that the process exits even if there are any hanging promises
    process.exit(0);
  }
})();
