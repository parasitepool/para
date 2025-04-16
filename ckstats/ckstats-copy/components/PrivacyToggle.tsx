'use client';

import React from 'react';

import { useMutation } from '@tanstack/react-query';

interface PrivacyToggleProps {
  address: string;
  initialIsPublic: boolean;
}

const PrivacyToggle: React.FC<PrivacyToggleProps> = ({
  address,
  initialIsPublic,
}) => {
  const [isPublic, setIsPublic] = React.useState(initialIsPublic);

  const mutation = useMutation({
    mutationFn: async () => {
      const response = await fetch(
        `/api/users/togglePrivacy?address=${address}`,
        {
          method: 'POST',
        }
      );
      if (!response.ok) {
        throw new Error('Failed to toggle privacy');
      }
      return response.json();
    },
    onSuccess: (data) => {
      setIsPublic(data.isPublic);
    },
    onError: (error) => {
      console.error('Error toggling privacy:', error);
    },
  });

  return (
    <div>
      <button
        className={`btn btn-sm btn-primary ${isPublic ? '' : 'btn-outline'}`}
        onClick={() => mutation.mutate()}
        disabled={mutation.isPending}
      >
        {isPublic ? 'Make Private' : 'Make Public'}
      </button>
      {mutation.isError && (
        <p className="text-error mt-2">Failed to privacy setting.</p>
      )}
    </div>
  );
};

export default PrivacyToggle;
