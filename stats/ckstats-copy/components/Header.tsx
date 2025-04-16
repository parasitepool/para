'use client';

import { useState, useRef } from 'react';

import { useMutation } from '@tanstack/react-query';
import Link from 'next/link';
import { useRouter } from 'next/navigation';

import ThemeController from './ThemeController';

export default function Header() {
  const [address, setAddress] = useState('');
  const [modalMessage, setModalMessage] = useState('');
  const [isError, setIsError] = useState(false);
  const modalRef = useRef<HTMLDialogElement>(null);
  const router = useRouter();

  const addUserMutation = useMutation({
    mutationFn: async (address: string) => {
      const response = await fetch('/api/users', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({ address }),
      });
      if (!response.ok) {
        const errorData = await response.json();
        throw new Error(errorData.error || 'Failed to add address');
      }
      return response.json();
    },
    onSuccess: (response) => {
      if (response?.message === 'Already in database') {
        setAddress('');
        setModalMessage('');
        setIsError(false);
        router.push(`/users/${address}`);
      } else {
        setAddress('');
        setModalMessage('Address added successfully!');
        setIsError(false);
        modalRef.current?.showModal();
        setTimeout(() => {
          router.push(`/users/${address}`);
        }, 1500);
      }
    },
    onError: (error: Error) => {
      if (error.message === 'Bitcoin address already exists') {
        router.push(`/users/${address}`);
      } else {
        setModalMessage(error.message);
        setIsError(true);
        modalRef.current?.showModal();
      }
    },
  });

  const handleAddAddress = async () => {
    const trimmedAddress = address.trim();

    addUserMutation.mutate(trimmedAddress);
  };

  return (
    <header className="navbar bg-base-100">
      <div className="flex-1 hidden md:inline-flex">
        <Link href="/" className="btn btn-ghost normal-case text-xl">
          Parasite Pool Stats
        </Link>
      </div>
      <div className="flex-none gap-1 sm:gap-2 flex-grow md:flex-grow-0">
        <div className="form-control flex-grow md:flex-grow-0">
          <input
            type="text"
            placeholder="Enter Bitcoin address"
            className="input input-bordered w-full md:w-96 text-sm"
            value={address}
            onChange={(e) => setAddress(e.target.value.trim())}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                e.preventDefault();
                handleAddAddress();
              }
            }}
          />
        </div>
        <button className="btn p-3 btn-primary" onClick={handleAddAddress}>
          <svg
            xmlns="http://www.w3.org/2000/svg"
            className="h-6 w-6"
            fill="none"
            viewBox="0 0 24 24"
            stroke="currentColor"
          >
            <path
              strokeLinecap="round"
              strokeLinejoin="round"
              strokeWidth={2}
              d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z"
            />
          </svg>
        </button>
        <ThemeController />
      </div>

      {/* DaisyUI Modal */}
      <dialog ref={modalRef} className="modal modal-bottom sm:modal-middle">
        <form method="dialog" className="modal-box">
          <h3
            className={`font-bold text-lg ${
              isError ? 'text-error' : 'text-success'
            }`}
          >
            {isError ? 'Error' : 'Success'}
          </h3>
          <p className="py-4">{modalMessage}</p>
          <div className="modal-action">
            <button className="btn">Close</button>
          </div>
        </form>
        <form method="dialog" className="modal-backdrop">
          <button>close</button>
        </form>
      </dialog>
    </header>
  );
}
