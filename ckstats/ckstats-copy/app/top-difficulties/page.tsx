export const revalidate = 60;

import React from 'react';

import TopUserDifficulties from '../../components/TopUserDifficulties';

export const metadata = {
  title: 'Top 100 User Difficulties - Parasite Pool',
  description: 'View the top 100 user difficulties on Parasite Pool.',
};

export default function TopDifficultiesPage() {
  return (
    <div className="container mx-auto p-4">
      <TopUserDifficulties limit={100} />
    </div>
  );
}
