'use client';

import React, { useState, useMemo } from 'react';

import { UserStats, WorkerStats } from '@prisma/client';
import {
  LineChart,
  Line,
  XAxis,
  YAxis,
  Tooltip,
  Legend,
  ResponsiveContainer,
  LegendType,
  Brush,
} from 'recharts';

// Add this function at the top of the file, outside the component
function getHashrateUnit(maxHashrate: number): [string, number] {
  if (maxHashrate >= 1e16) return ['PH/s', 1e15];
  if (maxHashrate >= 1e13) return ['TH/s', 1e12];
  if (maxHashrate >= 1e10) return ['GH/s', 1e9];
  if (maxHashrate >= 1e7) return ['MH/s', 1e6];
  if (maxHashrate >= 1e4) return ['KH/s', 1e3];
  return ['H/s', 1];
}

interface UserStatsChartsProps {
  userStats: (UserStats | WorkerStats)[];
}

export default function UserStatsCharts({ userStats }: UserStatsChartsProps) {
  const [hashrateUnit, setHashrateUnit] = useState<string>('PH/s');

  const chartData = useMemo(() => {
    // Reverse the userStats array
    const reversedStats = [...userStats].reverse();

    const maxHashrate = Math.max(
      ...reversedStats.flatMap((stat) => [
        Number(stat.hashrate1m),
        Number(stat.hashrate5m),
        Number(stat.hashrate1hr),
        Number(stat.hashrate1d),
        Number(stat.hashrate7d),
      ])
    );

    const [unit, scaleFactor] = getHashrateUnit(maxHashrate);
    setHashrateUnit(unit);

    return reversedStats.map((stat) => ({
      timestamp: new Date(stat.timestamp).toLocaleString('en-US', {
        month: 'short',
        day: 'numeric',
        hour: '2-digit',
        minute: '2-digit',
      }),
      workerCount: 'workerCount' in stat ? stat.workerCount : undefined,
      '1m': Number(stat.hashrate1m) / scaleFactor,
      '5m': Number(stat.hashrate5m) / scaleFactor,
      '1hr': Number(stat.hashrate1hr) / scaleFactor,
      '1d': Number(stat.hashrate1d) / scaleFactor,
      '7d': Number(stat.hashrate7d) / scaleFactor,
    }));
  }, [userStats]);

  const [visibleLines, setVisibleLines] = useState({
    '1m': false,
    '5m': true,
    '1hr': true,
    '1d': true,
    '7d': false,
  });

  const handleLegendClick = (dataKey: string) => {
    setVisibleLines((prev) => ({ ...prev, [dataKey]: !prev[dataKey] }));
  };

  const hashrateTooltipFormatter = (value: number, name: string) => [
    `${value.toLocaleString(undefined, { maximumFractionDigits: 2 })} ${hashrateUnit}`,
    name,
  ];

  const legendPayload = [
    {
      value: '1m',
      type: 'line',
      color: visibleLines['1m'] ? '#8884d8' : '#aaaaaa',
      formatter: (value: string) =>
        visibleLines['1m'] ? (
          <span style={{ cursor: 'pointer' }}>{value}</span>
        ) : (
          <span style={{ fontStyle: 'italic', cursor: 'pointer' }}>
            {value}
          </span>
        ),
    },
    {
      value: '5m',
      type: 'line',
      color: visibleLines['5m'] ? '#82ca9d' : '#aaaaaa',
      formatter: (value: string) =>
        visibleLines['5m'] ? (
          <span style={{ cursor: 'pointer' }}>{value}</span>
        ) : (
          <span style={{ fontStyle: 'italic', cursor: 'pointer' }}>
            {value}
          </span>
        ),
    },
    {
      value: '1hr',
      type: 'line',
      color: visibleLines['1hr'] ? '#ffc658' : '#aaaaaa',
      formatter: (value: string) =>
        visibleLines['1hr'] ? (
          <span style={{ cursor: 'pointer' }}>{value}</span>
        ) : (
          <span style={{ fontStyle: 'italic', cursor: 'pointer' }}>
            {value}
          </span>
        ),
    },
    {
      value: '1d',
      type: 'line',
      color: visibleLines['1d'] ? '#ff7300' : '#aaaaaa',
      formatter: (value: string) =>
        visibleLines['1d'] ? (
          <span style={{ cursor: 'pointer' }}>{value}</span>
        ) : (
          <span style={{ fontStyle: 'italic', cursor: 'pointer' }}>
            {value}
          </span>
        ),
    },
    {
      value: '7d',
      type: 'line',
      color: visibleLines['7d'] ? '#a4de6c' : '#aaaaaa',
      formatter: (value: string) =>
        visibleLines['7d'] ? (
          <span style={{ cursor: 'pointer' }}>{value}</span>
        ) : (
          <span style={{ fontStyle: 'italic', cursor: 'pointer' }}>
            {value}
          </span>
        ),
    },
  ];

  const workerCountChanged = useMemo(() => {
    if (!('workerCount' in userStats[0])) return false;
    const firstWorkerCount = userStats[0].workerCount;
    return userStats.some(
      (stat) => 'workerCount' in stat && stat.workerCount !== firstWorkerCount
    );
  }, [userStats]);

  return (
    <div className="space-y-8 mt-4">
      <div>
        <h2 className="text-xl font-bold mb-4">
          Hashrate History ({hashrateUnit})
        </h2>
        <ResponsiveContainer width="100%" height={300}>
          <LineChart
            data={chartData}
            margin={{ top: 5, right: 0, left: 0, bottom: 5 }}
          >
            <XAxis dataKey="timestamp" minTickGap={50} />
            <YAxis
              allowDataOverflow={true}
              domain={[
                (dataMin: number) => Math.floor(dataMin * 0.99),
                (dataMax: number) => Math.ceil(dataMax * 1.01),
              ]}
            />
            <Tooltip formatter={hashrateTooltipFormatter} />
            <Legend
              payload={legendPayload.map((item) => ({
                ...item,
                type: item.type as LegendType,
              }))}
              onClick={(e) => handleLegendClick(e.value)}
            />
            <Brush
              dataKey="timestamp"
              height={30}
              alwaysShowText={true}
              startIndex={
                chartData.length - 1440 > 0 ? chartData.length - 1440 : 0
              }
            />
            {visibleLines['1m'] && (
              <Line
                type="monotone"
                dataKey="1m"
                stroke="#8884d8"
                dot={false}
                isAnimationActive={false}
              />
            )}
            {visibleLines['5m'] && (
              <Line
                type="monotone"
                dataKey="5m"
                stroke="#82ca9d"
                dot={false}
                isAnimationActive={false}
              />
            )}
            {visibleLines['1hr'] && (
              <Line
                type="monotone"
                dataKey="1hr"
                stroke="#ffc658"
                dot={false}
                isAnimationActive={false}
              />
            )}
            {visibleLines['1d'] && (
              <Line
                type="monotone"
                dataKey="1d"
                stroke="#ff7300"
                dot={false}
                isAnimationActive={false}
              />
            )}
            {visibleLines['7d'] && (
              <Line
                type="monotone"
                dataKey="7d"
                stroke="#a4de6c"
                dot={false}
                isAnimationActive={false}
              />
            )}
          </LineChart>
        </ResponsiveContainer>
      </div>

      {'workerCount' in userStats[0] && workerCountChanged && (
        <div>
          <h2 className="text-xl font-bold mb-4">Worker Count History</h2>
          <ResponsiveContainer width="100%" height={300}>
            <LineChart
              data={chartData}
              margin={{ top: 5, right: 0, left: 0, bottom: 5 }}
            >
              <XAxis dataKey="timestamp" minTickGap={50} />
              <YAxis
                allowDataOverflow={true}
                domain={[
                  (dataMin: number) => Math.floor(dataMin * 0.99),
                  (dataMax: number) => Math.ceil(dataMax * 1.01),
                ]}
              />
              <Tooltip />
              <Legend />
              <Line
                type="monotone"
                dataKey="workerCount"
                stroke="#8884d8"
                name="Workers"
                dot={false}
                isAnimationActive={false}
              />
              <Brush
                dataKey="timestamp"
                height={30}
                alwaysShowText={true}
                startIndex={
                  chartData.length - 1440 > 0 ? chartData.length - 1440 : 0
                }
              />
            </LineChart>
          </ResponsiveContainer>
        </div>
      )}
    </div>
  );
}
