export const revalidate = 60;

import PoolStatsChart from '../components/PoolStatsChart';
import PoolStatsDisplay from '../components/PoolStatsDisplay';
import TopUserDifficulties from '../components/TopUserDifficulties';
import TopUserHashrates from '../components/TopUserHashrates';
import { getLatestPoolStats, getHistoricalPoolStats } from '../lib/api';

export default async function Home() {
  try {
    const [latestStats, historicalStats] = await Promise.all([
      getLatestPoolStats(),
      getHistoricalPoolStats(),
    ]);

    if (!latestStats) {
      return (
        <main className="container mx-auto p-4">
          <p>No stats available at the moment. Please try again later.</p>
        </main>
      );
    }

    return (
      <main className="container mx-auto p-4">
        <PoolStatsDisplay
          stats={latestStats}
          historicalStats={historicalStats}
        />
        {historicalStats && historicalStats.length > 0 ? (
          <PoolStatsChart data={historicalStats} />
        ) : (
          <p>Historical data is not available.</p>
        )}
        <div className="grid grid-cols-1 md:grid-cols-2 gap-4 mt-8">
          <TopUserDifficulties />
          <TopUserHashrates />
        </div>
      </main>
    );
  } catch (error) {
    console.error('Error fetching pool stats:', error);
    return (
      <main className="container mx-auto p-4">
        <p>
          An error occurred while fetching the stats. Please try again later.
        </p>
      </main>
    );
  }
}
