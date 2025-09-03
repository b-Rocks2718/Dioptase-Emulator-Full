  .kernel

EXIT:
  mode halt

_start:
  call far_label
  sys  EXIT # should return 42

  sys  EXIT
  sys  EXIT
  sys  EXIT

far_label:
  add  r3 r0 21
  add  r3 r3 21
  ret  

  sys  EXIT
  sys  EXIT
  sys  EXIT