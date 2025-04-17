'use client';

import React from 'react';

import { useMutation } from '@tanstack/react-query';

interface UserResetButtonProps {
  address: string;
}

const UserResetButton: React.FC<UserResetButtonProps> = ({ address }) => {
  const mutation = useMutation({
    mutationFn: async () => {
      try {
        const response = await fetch(`/api/resetUser?address=${address}`, {
          method: 'POST',
        });
        if (!response.ok) {
          throw new Error('Failed to reset user');
        }
        return response.json();
      } catch (err: unknown) {
        if (err instanceof Error) {
          throw new Error(err.message || 'Failed to reset user');
        }
        throw new Error('Failed to reset user');
      }
    },
    onSuccess: () => {
      // Optionally, you can handle the successful reset here
      // For example, you could refresh the page or update the UI
      window.location.reload(); // This will refresh the page
    },
    onError: (error: Error) => {
      console.error('Error resetting user:', error);
    },
  });

  return (
    <div>
      <button
        className="btn btn-error btn-sm"
        onClick={() => mutation.mutate()}
        disabled={mutation.isPending || mutation.isSuccess}
      >
        {mutation.isPending
          ? 'Resetting...'
          : mutation.isSuccess
            ? 'Reset Success'
            : 'Reset User'}
      </button>
      {mutation.isError && (
        <p className="text-error mt-2">
          {mutation.error instanceof Error
            ? mutation.error.message
            : 'Failed to reset user. Please try again.'}
        </p>
      )}
    </div>
  );
};

export default UserResetButton;
