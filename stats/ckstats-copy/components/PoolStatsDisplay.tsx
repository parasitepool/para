import dynamic from 'next/dynamic';
import Link from 'next/link';

import { PoolStatsType } from '../lib/api';
import {
  formatNumber,
  formatHashrate,
  formatTimeAgo,
  formatDuration,
  calculatePercentageChange,
  getPercentageChangeColor,
  calculateAverageTimeToBlock,
} from '../utils/helpers';

const CountdownTimer = dynamic(() => import('./CountdownTimer'), {
  ssr: false,
});

interface PoolStatsDisplayProps {
  stats: PoolStatsType;
  historicalStats: PoolStatsType[];
}

export default function PoolStatsDisplay({
  stats,
  historicalStats,
}: PoolStatsDisplayProps) {
  // Helper function to format values
  const formatValue = (key: string, value: any): string => {
    if (key.startsWith('hashrate')) {
      return formatHashrate(value);
    } else if (key === 'diff') {
      return `${formatNumber(value)}%`;
      // const networkDiff = (stats.accepted * BigInt(10000)) / BigInt(Math.round(Number(stats.diff) * 100));
      // return `${(Number(networkDiff) / 1e12).toFixed(2)}T`;
    } else if (typeof value === 'bigint' || typeof value === 'number') {
      return formatNumber(value);
    } else if (key === 'timestamp') {
      return new Date(value).toISOString().slice(0, 19) + ' UTC';
    }
    return String(value);
  };

  // Helper function to format keys
  const formatKey = (key: string): string => {
    // Handle hashrate and SPS cases
    if (key.startsWith('hashrate') || key.startsWith('SPS')) {
      return key.replace(/^(hashrate|SPS)/, '').toUpperCase();
    } else if (key === 'diff') {
      return '% of Network Diff';
    } else if (key === 'bestshare') {
      return 'Best Diff';
    }
    // General case
    return key
      .replace(/([A-Z])/g, ' $1')
      .replace(/^./, (str) => str.toUpperCase());
  };

  const statGroups = [
    { title: 'Users', keys: ['users', 'disconnected', 'workers'] },
    {
      title: 'Shares since last found block',
      keys: ['accepted', 'rejected', 'bestshare', 'diff'],
    },
    { title: 'Shares Per Second', keys: ['SPS1m', 'SPS5m', 'SPS15m', 'SPS1h'] },
  ];

  const hashrateGroup = {
    title: 'Hashrates',
    keys: [
      'hashrate1m',
      'hashrate5m',
      'hashrate15m',
      'hashrate1hr',
      'hashrate6hr',
      'hashrate1d',
      'hashrate7d',
    ],
  };

  const renderPercentageChange = (key: string) => {
    if (historicalStats.length < 120) return 'N/A';

    const currentValue = Number(stats[key]);
    const pastValue = Number(
      historicalStats[historicalStats.length - 120][key]
    );

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
    <div className="grid grid-cols-1 gap-4">
      <div className="grid grid-cols-1 md:grid-cols-2 gap-2">
        <div className="card card-compact">
          <div className="card-body">
            <div className="flex items-center justify-between gap-2">
              <h2 className="card-title">General Info</h2>
              <CountdownTimer initialSeconds={60} />
            </div>
            <div className="stats stats-vertical xl:stats-horizontal shadow-lg my-2">
              <div className="stat">
                <div className="stat-title">Uptime</div>
                <div className="stat-value text-2xl">
                  {formatDuration(stats.runtime)}
                </div>
              </div>
              <div className="stat">
                <div className="stat-title">Last Update</div>
                <div className="stat-value text-2xl">
                  {formatTimeAgo(stats.timestamp)}
                </div>
              </div>
              <div className="stat">
                <div className="stat-title">Avg Time to Find a Block</div>
                <div className="stat-value text-2xl">
                  {stats.hashrate6hr && stats.diff
                    ? formatDuration(
                        calculateAverageTimeToBlock(
                          stats.hashrate6hr,
                          (stats.accepted * BigInt(10000)) /
                            BigInt(Math.round(Number(stats.diff) * 100))
                        )
                      )
                    : 'N/A'}
                </div>
                <div className="stat-desc">
                  <Link
                    href="https://mempool.space/mining/pool/solock"
                    target="_blank"
                    className="link text-primary"
                  >
                    Found Blocks
                  </Link>
                </div>
              </div>
            </div>
          </div>
        </div>
        {statGroups.map((group) => (
          <div key={group.title} className="card card-compact">
            <div className="card-body">
              <h2 className="card-title">{group.title}</h2>
              <div className="stats stats-vertical lg:stats-horizontal shadow-lg my-2">
                {group.keys.map((key) => (
                  <div key={key} className="stat">
                    <div className="stat-title">{formatKey(key)}</div>
                    <div className="stat-value text-2xl">
                      {formatValue(key, stats[key])}
                    </div>
                    {key === 'users' && (
                      <div className="stat-desc">
                        Idle: {formatNumber(stats.idle)}
                      </div>
                    )}
                  </div>
                ))}
              </div>
            </div>
          </div>
        ))}
      </div>
      <div className="card card-compact">
        <div className="card-body">
          <h2 className="card-title">{hashrateGroup.title}</h2>
          <div className="stats stats-vertical lg:stats-horizontal shadow-lg my-2">
            {hashrateGroup.keys.map((key) => (
              <div key={key} className="stat">
                <div className="stat-title">{formatKey(key)}</div>
                <div className="stat-value text-2xl">
                  {formatValue(key, stats[key])}
                </div>
                {renderPercentageChange(key)}
              </div>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}
