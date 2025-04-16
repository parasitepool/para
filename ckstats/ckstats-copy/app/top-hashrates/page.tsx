export const revalidate = 60;

import React from 'react';

import TopUserHashrates from '../../components/TopUserHashrates';

export const metadata = {
  title: 'Top 100 User Hashrates - Parasite Pool',
  description: 'View the top 100 user hashrates on Parasite Pool',
};

export default function TopHashratesPage() {
  return (
    <div className="container mx-auto p-4">
      <TopUserHashrates limit={100} />
    </div>
  );
}
