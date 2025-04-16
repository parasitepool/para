import './globals.css';
import { Metadata } from 'next';
import { Lato } from 'next/font/google';

import Footer from '../components/Footer';
import Header from '../components/Header';
import Providers from '../components/Providers';

const lato = Lato({ subsets: ['latin'], weight: ['400', '700'] });

export const metadata: Metadata = {
  title: 'Parasite Pool Stats',
  description:
    'Real-time and historical statistics for the Parasite Pool Bitcoin mining pool using data from their API.',
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en">
      <head>
        <link rel="icon" href="/favicon.png" type="image/png" />
      </head>
      <body className={lato.className}>
        <Providers>
          <div className="flex flex-col min-h-screen">
            <Header />
            <main className="flex-grow">{children}</main>
            <Footer />
          </div>
        </Providers>
      </body>
    </html>
  );
}
