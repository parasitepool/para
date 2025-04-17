export const revalidate = 60;

import Link from 'next/link';
import { notFound } from 'next/navigation';

import UserStatsCharts from '../../../../../components/UserStatsCharts';
import { getWorkerWithStats } from '../../../../../lib/api';
import {
  formatHashrate,
  formatNumber,
  calculatePercentageChange,
  getPercentageChangeColor,
} from '../../../../../utils/helpers';

export default async function WorkerPage({
  params,
}: {
  params: { address: string; name?: string[] };
}) {
  const decodedName = params.name ? decodeURIComponent(params.name[0]) : '';
  const worker = await getWorkerWithStats(params.address, decodedName);

  if (!worker) {
    notFound();
  }

  const latestStats = worker.stats[0]; // Assuming stats are ordered by timestamp desc

  const renderPercentageChange = (key: string) => {
    if (worker.stats.length < 120) return 'N/A';

    const currentValue = Number(latestStats[key]);
    const pastValue = Number(worker.stats[worker.stats.length - 120][key]);

    const change = calculatePercentageChange(currentValue, pastValue);
    const color = getPercentageChangeColor(change);

    return (
      <div
        className={`stat-desc tooltip text-left ${color}`}
        data-tip="24 hour % change"
      >
        {change === 'N/A' ? 'N/A' : `${change}%`}
      </div>
    );
  };

  return (
    <div className="container mx-auto p-4">
      <div className="flex items-center gap-2">
        <Link href={`/users/${params.address}`} className="text-sm btn">
          {/* Inline SVG for Arrow Left Icon */}
          <svg
            className="w-4 h-4"
            fill="none"
            stroke="currentColor"
            viewBox="0 0 24 24"
            xmlns="http://www.w3.org/2000/svg"
          >
            <path
              strokeLinecap="round"
              strokeLinejoin="round"
              strokeWidth={2}
              d="M15 19l-7-7 7-7"
            />
          </svg>
          Back to User
        </Link>
        <h1 className="text-3xl font-bold text-accent">
          {worker.name || <span className="italic">Unnamed Worker</span>}
        </h1>
      </div>

      <div className="stats stats-vertical lg:stats-horizontal shadow-lg my-2">
        <div className="stat">
          <div className="stat-title">Hashrate (1m)</div>
          <div className="stat-value text-3xl">
            {formatHashrate(latestStats.hashrate1m)}
          </div>
          {renderPercentageChange('hashrate1m')}
        </div>

        <div className="stat">
          <div className="stat-title">Hashrate (5m) </div>
          <div className="stat-value text-3xl">
            {formatHashrate(latestStats.hashrate5m)}
          </div>
          {renderPercentageChange('hashrate5m')}
        </div>

        <div className="stat">
          <div className="stat-title">Hashrate (1hr)</div>
          <div className="stat-value text-3xl">
            {formatHashrate(latestStats.hashrate1hr)}
          </div>
          {renderPercentageChange('hashrate1hr')}
        </div>
        <div className="stat">
          <div className="stat-title">Hashrate (1d)</div>
          <div className="stat-value text-3xl">
            {formatHashrate(latestStats.hashrate1d)}
          </div>
          {renderPercentageChange('hashrate1d')}
        </div>
        <div className="stat">
          <div className="stat-title">Hashrate (7d)</div>
          <div className="stat-value text-3xl">
            {formatHashrate(latestStats.hashrate7d)}
          </div>
          {renderPercentageChange('hashrate7d')}
        </div>
      </div>

      <div className="stats stats-vertical lg:stats-horizontal shadow-lg my-2">
        <div className="stat">
          <div className="stat-title">Shares</div>
          <div className="stat-value text-3xl">
            {formatNumber(worker.shares)}
          </div>
        </div>
        <div className="stat">
          <div className="stat-title">Best Share</div>
          <div className="stat-value text-3xl">
            {formatNumber(worker.bestShare)}
          </div>
        </div>
        <div className="stat">
          <div className="stat-title">Best Ever</div>
          <div className="stat-value text-3xl">
            {formatNumber(worker.bestEver)}
          </div>
        </div>
      </div>

      <div className="mt-8">
        <UserStatsCharts userStats={worker.stats} />
      </div>
    </div>
  );
}
