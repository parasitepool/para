'use client';

import { useState } from 'react';

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

import { PoolStatsType } from '../lib/api';

interface PoolStatsChartProps {
  data: PoolStatsType[];
}

export default function PoolStatsChart({ data }: PoolStatsChartProps) {
  const [visibleLines, setVisibleLines] = useState({
    '1m': false,
    '5m': true,
    '15m': true,
    '1hr': true,
    '6hr': true,
    '1d': true,
    '7d': true,
  });

  const handleLegendClick = (dataKey: string) => {
    setVisibleLines((prev) => ({ ...prev, [dataKey]: !prev[dataKey] }));
  };

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
      value: '15m',
      type: 'line',
      color: visibleLines['15m'] ? '#ffc658' : '#aaaaaa',
      formatter: (value: string) =>
        visibleLines['15m'] ? (
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
      color: visibleLines['1hr'] ? '#ff7300' : '#aaaaaa',
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
      value: '6hr',
      type: 'line',
      color: visibleLines['6hr'] ? '#00C49F' : '#aaaaaa',
      formatter: (value: string) =>
        visibleLines['6hr'] ? (
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
      color: visibleLines['1d'] ? '#0088FE' : '#aaaaaa',
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
      color: visibleLines['7d'] ? '#FF1493' : '#aaaaaa',
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

  // Reverse the data array
  const reversedData = [...data].reverse();

  // Format the reversed data for the charts
  const formattedData = reversedData.map((item) => ({
    ...item,
    timestamp: new Date(item.timestamp).toLocaleString('en-US', {
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit',
    }),
    hashrate1m: Number(item.hashrate1m) / 1000000000000000,
    hashrate5m: Number(item.hashrate5m) / 1000000000000000,
    hashrate15m: Number(item.hashrate15m) / 1000000000000000,
    hashrate1hr: Number(item.hashrate1hr) / 1000000000000000,
    hashrate6hr: Number(item.hashrate6hr) / 1000000000000000,
    hashrate1d: Number(item.hashrate1d) / 1000000000000000,
    hashrate7d: Number(item.hashrate7d) / 1000000000000000,
    SPS1m: item.SPS1m ?? 0,
    SPS5m: item.SPS5m ?? 0,
    SPS15m: item.SPS15m ?? 0,
    SPS1h: item.SPS1h ?? 0,
  }));

  const hashrateTooltipFormatter = (value: number, name: string) => [
    `${value.toLocaleString(undefined, { maximumFractionDigits: 1 })} PH/s`,
    name,
  ];

  const spsTooltipFormatter = (value: number, name: string) => [
    `${value.toFixed(0)} SPS`,
    name,
  ];

  const renderUsersChart = () => (
    <div className="h-80 w-full mb-8">
      <h2 className="text-xl font-bold mb-2">Users and Workers</h2>
      <ResponsiveContainer width="100%" height="100%">
        <LineChart
          data={formattedData}
          margin={{ top: 5, right: 0, left: 0, bottom: 5 }}
        >
          <XAxis dataKey="timestamp" minTickGap={40} />
          <YAxis
            yAxisId="left"
            allowDataOverflow={true}
            domain={[
              (dataMin: number) => Math.floor(dataMin * 0.99),
              (dataMax: number) => Math.ceil(dataMax * 1.01),
            ]}
          />
          <YAxis
            yAxisId="right"
            orientation="right"
            allowDataOverflow={true}
            domain={[
              (dataMin: number) => Math.floor(dataMin * 0.99),
              (dataMax: number) => Math.ceil(dataMax * 1.01),
            ]}
          />
          <Tooltip />
          <Legend />
          <Brush
            dataKey="timestamp"
            height={30}
            alwaysShowText={true}
            startIndex={
              formattedData.length - 1440 > 0 ? formattedData.length - 1440 : 0
            }
          />
          <Line
            yAxisId="left"
            type="monotone"
            dataKey="users"
            stroke="#8884d8"
            activeDot={{ r: 8 }}
            name="Users"
            dot={false}
            isAnimationActive={false}
          />
          <Line
            yAxisId="right"
            type="monotone"
            dataKey="workers"
            stroke="#82ca9d"
            activeDot={{ r: 8 }}
            name="Workers"
            dot={false}
            isAnimationActive={false}
          />
        </LineChart>
      </ResponsiveContainer>
    </div>
  );

  const renderHashrateChart = () => (
    <div className="h-80 w-full mb-8">
      <h2 className="text-xl font-bold mb-2">Hashrate (PH/s)</h2>
      <ResponsiveContainer width="100%" height="100%">
        <LineChart
          data={formattedData}
          margin={{ top: 5, right: 30, left: 0, bottom: 5 }}
        >
          <XAxis dataKey="timestamp" minTickGap={40} />
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
              formattedData.length - 1440 > 0 ? formattedData.length - 1440 : 0
            }
          />
          {visibleLines['1m'] && (
            <Line
              type="monotone"
              dataKey="hashrate1m"
              name="1M"
              stroke="#8884d8"
              dot={false}
              isAnimationActive={false}
            />
          )}
          {visibleLines['5m'] && (
            <Line
              type="monotone"
              dataKey="hashrate5m"
              name="5M"
              stroke="#82ca9d"
              dot={false}
              isAnimationActive={false}
            />
          )}
          {visibleLines['15m'] && (
            <Line
              type="monotone"
              dataKey="hashrate15m"
              name="15M"
              stroke="#ffc658"
              dot={false}
              isAnimationActive={false}
            />
          )}
          {visibleLines['1hr'] && (
            <Line
              type="monotone"
              dataKey="hashrate1hr"
              name="1HR"
              stroke="#ff7300"
              dot={false}
              isAnimationActive={false}
            />
          )}
          {visibleLines['6hr'] && (
            <Line
              type="monotone"
              dataKey="hashrate6hr"
              name="6HR"
              stroke="#00C49F"
              dot={false}
              isAnimationActive={false}
            />
          )}
          {visibleLines['1d'] && (
            <Line
              type="monotone"
              dataKey="hashrate1d"
              name="1D"
              stroke="#0088FE"
              dot={false}
              isAnimationActive={false}
            />
          )}
          {visibleLines['7d'] && (
            <Line
              type="monotone"
              dataKey="hashrate7d"
              name="7D"
              stroke="#FF1493"
              dot={false}
              isAnimationActive={false}
            />
          )}
        </LineChart>
      </ResponsiveContainer>
    </div>
  );

  const renderSPSChart = () => (
    <div className="h-80 w-full mb-8">
      <h2 className="text-xl font-bold mb-2">Shares Per Second</h2>
      <ResponsiveContainer width="100%" height="100%">
        <LineChart
          data={formattedData}
          margin={{ top: 5, right: 30, left: 0, bottom: 5 }}
        >
          <XAxis dataKey="timestamp" minTickGap={40} />
          <YAxis
            allowDataOverflow={true}
            domain={[
              (dataMin: number) => Math.floor(dataMin * 0.99),
              (dataMax: number) => Math.ceil(dataMax * 1.01),
            ]}
          />
          <Tooltip formatter={spsTooltipFormatter} />
          <Legend />
          <Brush
            dataKey="timestamp"
            height={30}
            alwaysShowText={true}
            startIndex={
              formattedData.length - 1440 > 0 ? formattedData.length - 1440 : 0
            }
          />
          <Line
            type="monotone"
            dataKey="SPS1m"
            name="1M"
            stroke="#8884d8"
            dot={false}
            isAnimationActive={false}
          />
          <Line
            type="monotone"
            dataKey="SPS5m"
            name="5M"
            stroke="#82ca9d"
            dot={false}
            isAnimationActive={false}
          />
          <Line
            type="monotone"
            dataKey="SPS15m"
            name="15M"
            stroke="#ffc658"
            dot={false}
            isAnimationActive={false}
          />
          <Line
            type="monotone"
            dataKey="SPS1h"
            name="1H"
            stroke="#ff7300"
            dot={false}
            isAnimationActive={false}
          />
        </LineChart>
      </ResponsiveContainer>
    </div>
  );

  return (
    <div className="mt-4">
      {renderUsersChart()}
      {renderHashrateChart()}
      {renderSPSChart()}
    </div>
  );
}
