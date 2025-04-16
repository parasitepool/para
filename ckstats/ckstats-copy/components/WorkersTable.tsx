'use client';

import React, { useState } from 'react';

import { Worker } from '@prisma/client';
import Link from 'next/link';

import { formatHashrate, formatNumber, formatTimeAgo } from '../utils/helpers';

interface WorkersTableProps {
  workers: Worker[];
  address: string;
}

type SortField = keyof Worker;
type SortOrder = 'asc' | 'desc';

const WorkersTable: React.FC<WorkersTableProps> = ({ workers, address }) => {
  const [sortField, setSortField] = useState<SortField>('hashrate5m');
  const [sortOrder, setSortOrder] = useState<SortOrder>('desc');

  const handleSort = (field: SortField) => {
    if (field === sortField) {
      setSortOrder(sortOrder === 'asc' ? 'desc' : 'asc');
    } else {
      setSortField(field);
      setSortOrder('desc');
    }
  };

  const sortedWorkers = [...workers].sort((a, b) => {
    if (sortField) {
      if (a[sortField] < b[sortField]) return sortOrder === 'asc' ? -1 : 1;
      if (a[sortField] > b[sortField]) return sortOrder === 'asc' ? 1 : -1;
    }
    return 0;
  });

  const renderSortIcon = (field: SortField) => {
    if (sortField !== field) return null;
    return sortOrder === 'asc' ? ' ▲' : ' ▼';
  };

  return (
    <div className="bg-base-200 p-4 rounded-lg mt-8">
      <h2 className="text-xl font-bold mb-4">Workers</h2>
      <div className="overflow-x-auto">
        <table className="table w-full table-sm sm:table-md">
          <thead>
            <tr>
              <th onClick={() => handleSort('name')} className="cursor-pointer">
                Name{renderSortIcon('name')}
              </th>
              <th
                onClick={() => handleSort('hashrate5m')}
                className="cursor-pointer"
              >
                Hashrate (5m){renderSortIcon('hashrate5m')}
              </th>
              <th
                onClick={() => handleSort('hashrate1hr')}
                className="cursor-pointer"
              >
                Hashrate (1hr){renderSortIcon('hashrate1hr')}
              </th>
              <th
                onClick={() => handleSort('hashrate1d')}
                className="cursor-pointer"
              >
                Hashrate (1d){renderSortIcon('hashrate1d')}
              </th>
              <th
                onClick={() => handleSort('bestShare')}
                className="cursor-pointer"
              >
                Best Share{renderSortIcon('bestShare')}
              </th>
              <th
                onClick={() => handleSort('bestEver')}
                className="cursor-pointer"
              >
                Best Ever{renderSortIcon('bestEver')}
              </th>
              <th
                onClick={() => handleSort('lastUpdate')}
                className="cursor-pointer"
              >
                Last Update{renderSortIcon('lastUpdate')}
              </th>
            </tr>
          </thead>
          <tbody>
            {sortedWorkers.map((worker) => (
              <tr key={worker.id}>
                <td>
                  <Link
                    className="link text-primary"
                    href={`/users/${address}/workers/${encodeURIComponent(worker.name)}`}
                  >
                    {worker.name || <span className="italic">Unnamed</span>}
                  </Link>
                </td>
                <td
                  className={`${worker.hashrate5m < 1 ? 'text-error' : 'text-accent'}`}
                >
                  {formatHashrate(worker.hashrate5m)}
                </td>
                <td className={`${worker.hashrate1hr < 1 ? 'text-error' : ''}`}>
                  {formatHashrate(worker.hashrate1hr)}
                </td>
                <td className={`${worker.hashrate1d < 1 ? 'text-error' : ''}`}>
                  {formatHashrate(worker.hashrate1d)}
                </td>
                <td>{formatNumber(worker.bestShare)}</td>
                <td>{formatNumber(worker.bestEver)}</td>
                <td>{formatTimeAgo(worker.lastUpdate)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
};

export default WorkersTable;
