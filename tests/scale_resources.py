#!/usr/bin/env python3
"""Native resource measurement, not an alternative ranking evaluator."""
import resource, subprocess, sys, time
start=time.perf_counter()
p=subprocess.run(sys.argv[1:],stdout=subprocess.DEVNULL,check=True)
print('elapsed_s: %.6f' % (time.perf_counter()-start))
print('RSS_kB: %d' % resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss)
