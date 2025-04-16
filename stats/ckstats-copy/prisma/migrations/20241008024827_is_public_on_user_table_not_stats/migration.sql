/*
  Warnings:

  - You are about to drop the column `isPublic` on the `UserStats` table. All the data in the column will be lost.

*/
-- AlterTable
ALTER TABLE "User" ADD COLUMN     "isPublic" BOOLEAN NOT NULL DEFAULT true;

-- AlterTable
ALTER TABLE "UserStats" DROP COLUMN "isPublic";
