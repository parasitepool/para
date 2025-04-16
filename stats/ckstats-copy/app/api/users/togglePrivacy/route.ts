import { NextRequest, NextResponse } from 'next/server';

import { toggleUserStatsPrivacy } from '../../../../lib/api';

export async function POST(request: NextRequest) {
  const { searchParams } = new URL(request.url);
  const address = searchParams.get('address');

  if (!address) {
    return NextResponse.json({ error: 'Address is required' }, { status: 400 });
  }

  try {
    const updatedUser = await toggleUserStatsPrivacy(address);
    return NextResponse.json({
      message: 'User privacy setting updated successfully',
      isPublic: updatedUser.isPublic,
    });
  } catch (error) {
    console.error('Error toggling user privacy:', error);
    return NextResponse.json(
      { error: 'Failed to update user privacy setting' },
      { status: 500 }
    );
  }
}
