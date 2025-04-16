'use client';

import { useEffect, useState } from 'react';

export default function CountdownTimer({
  initialSeconds,
}: {
  initialSeconds: number;
}) {
  const [seconds, setSeconds] = useState(initialSeconds);

  useEffect(() => {
    const timer = setInterval(() => {
      setSeconds((prevSeconds) => {
        if (prevSeconds <= 1) {
          clearInterval(timer);
          window.location.reload();
          return 0;
        }
        return prevSeconds - 1;
      });
    }, 1000);

    return () => clearInterval(timer);
  }, [initialSeconds]);

  if (seconds === 0) {
    return <div className="badge badge-primary">Updating Now</div>;
  }
  return (
    <div className="badge badge-primary whitespace-nowrap">
      Updating in {seconds}s
    </div>
  );
}
