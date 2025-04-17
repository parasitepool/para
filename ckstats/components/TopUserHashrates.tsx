export const revalidate = 60;

import React from 'react';

import Link from 'next/link';

import { getTopUserHashrates } from '../lib/api';
import { formatHashrate, formatNumber } from '../utils/helpers';

interface TopUserHashratesProps {
  limit?: number;
}

const SMALL_LIMIT = 10;

export default async function TopUserHashrates({
  limit = SMALL_LIMIT,
}: TopUserHashratesProps) {
  try {
    const hashrates = await getTopUserHashrates(limit);

    return (
      <div className="card bg-base-100 shadow-xl card-compact sm:card-normal">
        <div className="card-body">
          <h2 className="card-title">
            {limit > SMALL_LIMIT ? (
              `Top ${limit} User Hashrates`
            ) : (
              <Link href="/top-hashrates" className="link text-primary">
                Top {limit} User Hashrates
              </Link>
            )}
          </h2>
          <div className="overflow-x-auto">
            <table className="table w-full table-sm sm:table-md">
              <thead>
                <tr>
                  <th>Rank</th>
                  <th>Address</th>
                  {limit > SMALL_LIMIT ? (
                    <>
                      <th>Active Workers</th>
                      <th>Hashrate 1hr</th>
                      <th>Hashrate 1d</th>
                      <th>Hashrate 7d</th>
                      <th>Session Diff</th>
                      <th>Best Diff</th>
                    </>
                  ) : (
                    <th>Hashrate</th>
                  )}
                </tr>
              </thead>
              <tbody>
                {hashrates.map((user, index) => (
                  <tr key={user.address}>
                    <td>{index + 1}</td>
                    <td>
                      {user.address.slice(0, 6)}...{user.address.slice(-4)}
                    </td>

                    {limit > SMALL_LIMIT ? (
                      <>
                        <td>{user.workerCount}</td>
                        <td className="text-accent">
                          {formatHashrate(user.hashrate1hr)}
                        </td>
                        <td>{formatHashrate(user.hashrate1d)}</td>
                        <td>{formatHashrate(user.hashrate7d)}</td>
                        <td>{formatNumber(Number(user.bestShare))}</td>
                        <td>{formatNumber(Number(user.bestEver))}</td>
                      </>
                    ) : (
                      <td className="text-accent">
                        {formatHashrate(user.hashrate1hr)}
                      </td>
                    )}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      </div>
    );
  } catch (error) {
    console.error('Error fetching top user hashrates:', error);
    return (
      <div className="card bg-base-100 shadow-xl card-compact sm:card-normal">
        <div className="card-body">
          <h2 className="card-title">Top {limit} User Hashrates</h2>
          <p className="text-error">
            Error loading top user hashrates. Please try again later.
          </p>
        </div>
      </div>
    );
  }
}
