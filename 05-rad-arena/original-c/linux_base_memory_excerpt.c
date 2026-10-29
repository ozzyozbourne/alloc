// Excerpt from raddebugger src/linux/base/linux_base.c (lines 228-277): the reserve/commit layer arenas sit on.


internal void *
reserve_memory(U64 size)
{
  void *result = mmap(0, size, PROT_NONE, MAP_PRIVATE|MAP_ANONYMOUS, -1, 0);
  if(result == MAP_FAILED)
  {
    result = 0;
  }
  return result;
}

internal B32
commit_memory(void *ptr, U64 size)
{
  return mprotect(ptr, size, PROT_READ|PROT_WRITE) == 0;
}

internal void
decommit_memory(void *ptr, U64 size)
{
  madvise(ptr, size, MADV_DONTNEED);
  mprotect(ptr, size, PROT_NONE);
}

internal void
release_memory(void *ptr, U64 size)
{
  munmap(ptr, size);
}

//- rjf: large pages

internal void *
reserve_memory_large(U64 size)
{
  void *result = mmap(0, size, PROT_NONE, MAP_PRIVATE|MAP_ANONYMOUS|MAP_HUGETLB, -1, 0);
  if(result == MAP_FAILED)
  {
    result = 0;
  }
  return result;
}

internal B32
commit_memory_large(void *ptr, U64 size)
{
  mprotect(ptr, size, PROT_READ|PROT_WRITE);
  return 1;
}
