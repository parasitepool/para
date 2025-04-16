export const revalidate = 60;

import React from 'react';

import Link from 'next/link';

import { getTopUserDifficulties } from '../lib/api';
import { formatHashrate, formatNumber } from '../utils/helpers';

interface TopUserDifficultiesProps {
  limit?: number;
}

const SMALL_LIMIT = 10;

export default async function TopUserDifficulties({
  limit = SMALL_LIMIT,
}: TopUserDifficultiesProps) {
  try {
    const difficulties = await getTopUserDifficulties(limit);

    return (
      <div className="card bg-base-100 shadow-xl card-compact sm:card-normal">
        <div className="card-body">
          <h2 className="card-title">
            {limit > SMALL_LIMIT ? (
              `Top ${limit} User Difficulties Ever`
            ) : (
              <Link href="/top-difficulties" className="link text-primary">
                Top {limit} User Difficulties Ever
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
                      <th>Best Diff</th>
                      <th>Session Diff</th>
                      <th>Hashrate 1hr</th>
                      <th>Hashrate 1d</th>
                      <th>Hashrate 7d</th>
                    </>
                  ) : (
                    <th>Best Diff</th>
                  )}
                </tr>
              </thead>
              <tbody>
                {difficulties.map((user, index) => (
                  <tr key={user.address}>
                    <td>
                      {index + 1}{' '}
                      {user.workerCount === 0 ? (
                        <span
                          className="text-warning tooltip tooltip-right"
                          data-tip="No active workers"
                        >
                          <svg
                            xmlns="http://www.w3.org/2000/svg"
                            className="h-4 w-4 inline-block stroke-current"
                            fill="none"
                            viewBox="0 0 24 24"
                          >
                            <path
                              strokeLinecap="round"
                              strokeLinejoin="round"
                              strokeWidth="2"
                              d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z"
                            />
                          </svg>
                        </span>
                      ) : (
                        ''
                      )}
                    </td>
                    <td>
                      {user.address.slice(0, 6)}...{user.address.slice(-4)}
                    </td>

                    {limit > SMALL_LIMIT ? (
                      <>
                        <td
                          className={`${user.workerCount === 0 ? 'text-error' : ''}`}
                        >
                          {user.workerCount}
                        </td>
                        <td className="text-accent">
                          {formatNumber(Number(user.difficulty))}
                        </td>
                        <td>{formatNumber(Number(user.bestShare))}</td>
                        <td>{formatHashrate(user.hashrate1hr)}</td>
                        <td>{formatHashrate(user.hashrate1d)}</td>
                        <td>{formatHashrate(user.hashrate7d)}</td>
                      </>
                    ) : (
                      <td className="text-accent">
                        {formatNumber(Number(user.difficulty))}
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
    console.error('Error fetching top user difficulties:', error);
    return (
      <div className="card bg-base-100 shadow-xl card-compact sm:card-normal">
        <div className="card-body">
          <h2 className="card-title">Top {limit} User Difficulties</h2>
          <p className="text-error">
            Error loading top user difficulties. Please try again later.
          </p>
        </div>
      </div>
    );
  }
}
